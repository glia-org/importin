//! Parse a raw RFC 822 message with mail-parser: headers, the text body, the attachment parts with
//! their states, and the S/MIME shape (enveloped, signed).
//!
//! Parts are numbered IMAP-style ("1", "2", "2.1"). Only the outer message is walked: a
//! `message/rfc822` part is one attachment and is never descended into.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use mail_parser::{AddressList, MessageParser, MessagePart};
use serde::Serialize;

use crate::emlx::MAX_EMLX_BYTES;
use crate::envelope::Address;

#[derive(Debug, thiserror::Error)]
pub enum MimeError {
    #[error("not a parseable message")]
    Unparseable,
    #[error("the message has no part {0}")]
    NoPart(String),
    #[error("attachment is {0:?}: its bytes are not available")]
    Unavailable(AttachmentState),
    #[error("attachment path leaves the attachment directory: {}", .0.display())]
    Unsafe(PathBuf),
    #[error("attachment is {bytes} bytes, over the {max}-byte limit")]
    TooLarge { bytes: u64, max: u64 },
    #[error("attachment file: {0}")]
    Io(#[from] io::Error),
}

pub struct Parsed<'a> {
    msg: mail_parser::Message<'a>,
}

pub fn parse(raw: &[u8]) -> Result<Parsed<'_>, MimeError> {
    MessageParser::default()
        .parse(raw)
        .map(|msg| Parsed { msg })
        .ok_or(MimeError::Unparseable)
}

#[derive(Debug, Clone, Serialize)]
pub struct Headers {
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub date: Option<DateTime<FixedOffset>>,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// The top-level content type, lowercased, `text/plain` when absent.
    pub content_type: String,
    /// The top-level `smime-type` parameter, lowercased.
    pub smime_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BodySource {
    Plain,
    Html,
    None,
}

#[derive(Debug, Clone)]
pub struct Body {
    pub text: String,
    pub source: BodySource,
    /// The raw HTML part, when asked for and present, cut at `max_chars` like `text`.
    pub html: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentState {
    /// Bytes in the message body.
    Inline,
    /// Empty body, bytes in the message's `Attachments/` directory.
    External,
    /// Empty body, no file, `X-Apple-Content-Length` present: never fetched from the server.
    NotDownloaded,
    /// Empty body, no file, no length header.
    NotExtractable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentInfo {
    /// Position in the list `attachments` returned.
    pub index: usize,
    /// IMAP-style part number, "2" or "2.2".
    pub part: String,
    pub filename: String,
    pub mime_type: String,
    /// Decoded bytes (inline), file size (external), `X-Apple-Content-Length` (not downloaded), 0.
    pub size: u64,
    pub state: AttachmentState,
}

/// A non-multipart part of the outer message with its part number.
struct Leaf<'m> {
    number: String,
    part: MessagePart<'m>,
    /// A direct child of a `multipart/signed`.
    in_signed: bool,
}

impl<'a> Parsed<'a> {
    pub fn headers(&self) -> Headers {
        let m = &self.msg;
        let root = m.root_part();
        Headers {
            message_id: m.message_id().map(str::to_string),
            in_reply_to: m.in_reply_to().and_then(|l| l.first()).map(str::to_string),
            references: m
                .references()
                .map(|l| l.iter().map(str::to_string).collect())
                .unwrap_or_default(),
            subject: m.subject().unwrap_or_default().to_string(),
            date: m
                .date()
                .filter(|d| d.is_valid())
                .and_then(|d| DateTime::parse_from_rfc3339(&d.to_rfc3339()).ok()),
            from: addresses(m.from()),
            to: addresses(m.to()),
            cc: addresses(m.cc()),
            bcc: addresses(m.bcc()),
            reply_to: addresses(m.reply_to()),
            content_type: mime_type(&root),
            smime_type: smime_type(&root),
        }
    }

    /// The first text/plain part, else the first text/html part stripped. Parts with a filename or
    /// an attachment disposition are attachments, not body. `truncated`: text or html was cut.
    pub fn body(&self, max_chars: usize, want_html: bool) -> Body {
        let leaves = self.leaves();
        let first = |mime: &str| {
            leaves
                .iter()
                .find(|l| mime_type(&l.part) == mime && !named_or_attached(&l.part))
                .and_then(|l| l.part.text())
        };
        let plain = first("text/plain");
        let html = first("text/html");
        let (text, source) = match (&plain, &html) {
            (Some(p), _) => (p.to_string(), BodySource::Plain),
            (None, Some(h)) => (strip_html(h), BodySource::Html),
            (None, None) => (String::new(), BodySource::None),
        };
        let (text, text_cut) = truncate(text, max_chars);
        let (html, html_cut) = match html.filter(|_| want_html) {
            Some(h) => {
                let (h, cut) = truncate(h.into_owned(), max_chars);
                (Some(h), cut)
            }
            None => (None, false),
        };
        Body {
            text,
            source,
            html,
            truncated: text_cut || html_cut,
        }
    }

    pub fn attachments(&self, attachments_dir: Option<&Path>) -> Vec<AttachmentInfo> {
        self.leaves()
            .into_iter()
            .filter(is_attachment)
            .enumerate()
            .map(|(index, leaf)| {
                let mime_type = mime_type(&leaf.part);
                let filename = match leaf.part.attachment_name() {
                    Some(name) => name.to_string(),
                    None if mime_type == "message/rfc822" => "message.eml".to_string(),
                    None => format!("part-{}", leaf.number),
                };
                let (state, size) = state_of(&leaf, &filename, attachments_dir);
                AttachmentInfo {
                    index,
                    part: leaf.number,
                    filename,
                    mime_type,
                    size,
                    state,
                }
            })
            .collect()
    }

    pub fn attachment_bytes(
        &self,
        info: &AttachmentInfo,
        attachments_dir: Option<&Path>,
    ) -> Result<Vec<u8>, MimeError> {
        match info.state {
            AttachmentState::Inline => self
                .leaves()
                .into_iter()
                .find(|l| l.number == info.part)
                .map(|l| l.part.decoded().into_owned())
                .ok_or_else(|| MimeError::NoPart(info.part.clone())),
            AttachmentState::External => {
                let missing = || {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("{} is not in the attachment directory", info.filename),
                    )
                };
                let dir = attachments_dir.ok_or_else(missing)?;
                let path = find_external(dir, &info.part, &info.filename)?.ok_or_else(missing)?;
                let bytes = fs::metadata(&path)?.len();
                if bytes > MAX_EMLX_BYTES {
                    return Err(MimeError::TooLarge {
                        bytes,
                        max: MAX_EMLX_BYTES,
                    });
                }
                Ok(fs::read(path)?)
            }
            state => Err(MimeError::Unavailable(state)),
        }
    }

    pub fn is_enveloped(&self) -> bool {
        self.enveloped_part().is_some()
    }

    pub fn is_auth_enveloped(&self) -> bool {
        self.enveloped_part()
            .is_some_and(|p| smime_type(&p).as_deref() == Some("authenveloped-data"))
    }

    pub fn is_signed(&self) -> bool {
        let root = self.msg.root_part();
        let mime = mime_type(&root);
        mime == "multipart/signed"
            || (is_pkcs7_mime(&mime) && smime_type(&root).as_deref() == Some("signed-data"))
    }

    /// The transfer-decoded body of the enveloped part, when it is DER (starts with a SEQUENCE).
    pub fn enveloped_der(&self) -> Option<Vec<u8>> {
        let der = self.enveloped_part()?.decoded().into_owned();
        (der.first() == Some(&0x30)).then_some(der)
    }

    /// Headers and body of the first child of a top-level `multipart/signed`, as written.
    pub fn signed_first_part(&self) -> Option<&[u8]> {
        let root = self.msg.root_part();
        if mime_type(&root) != "multipart/signed" {
            return None;
        }
        root.children().next().map(|p| p.raw())
    }

    /// The root part, when it is pkcs7-mime with smime-type enveloped-data or authEnveloped-data,
    /// or without an smime-type and named `smime.p7m`. The name alone does not decide: RFC 5751
    /// names signed-data `smime.p7m` too. A `.p7m` deeper in the tree is an attachment.
    fn enveloped_part(&self) -> Option<MessagePart<'_>> {
        let root = self.msg.root_part();
        let enveloped = is_pkcs7_mime(&mime_type(&root))
            && match smime_type(&root).as_deref() {
                Some(t) => t == "enveloped-data" || t == "authenveloped-data",
                None => root
                    .attachment_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case("smime.p7m")),
            };
        enveloped.then_some(root)
    }

    fn leaves(&self) -> Vec<Leaf<'_>> {
        fn walk<'m>(part: MessagePart<'m>, number: &str, in_signed: bool, out: &mut Vec<Leaf<'m>>) {
            if part.is_multipart() {
                let signed = mime_type(&part) == "multipart/signed";
                for (i, child) in part.children().enumerate() {
                    let n = match number {
                        "" => (i + 1).to_string(),
                        _ => format!("{number}.{}", i + 1),
                    };
                    walk(child, &n, signed, out);
                }
            } else {
                let number = if number.is_empty() { "1" } else { number };
                out.push(Leaf {
                    number: number.to_string(),
                    part,
                    in_signed,
                });
            }
        }
        let mut out = Vec::new();
        walk(self.msg.root_part(), "", false, &mut out);
        out
    }
}

fn addresses(list: Option<AddressList<'_>>) -> Vec<Address> {
    list.map(|l| {
        l.mailboxes()
            .map(|mb| Address {
                name: mb.name().unwrap_or_default().to_string(),
                address: mb.address().unwrap_or_default().to_string(),
            })
            .collect()
    })
    .unwrap_or_default()
}

/// `type/subtype` lowercased; `text/plain` without a (parseable) Content-Type.
fn mime_type(part: &MessagePart<'_>) -> String {
    match part.content_type() {
        Some(ct) => match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub).to_ascii_lowercase(),
            None => ct.ctype().to_ascii_lowercase(),
        },
        None => "text/plain".to_string(),
    }
}

fn smime_type(part: &MessagePart<'_>) -> Option<String> {
    part.content_type()?
        .attribute("smime-type")
        .map(str::to_ascii_lowercase)
}

/// `application/pkcs7-mime`, or `x-pkcs7-mime` as older clients write it (RFC 5751 §3.7).
fn is_pkcs7_mime(mime: &str) -> bool {
    mime == "application/pkcs7-mime" || mime == "application/x-pkcs7-mime"
}

fn named_or_attached(part: &MessagePart<'_>) -> bool {
    part.attachment_name().is_some()
        || part
            .content_disposition()
            .is_some_and(|d| d.is_attachment())
}

/// Named or attached, or a non-text leaf; never a multipart, never the signature of a
/// `multipart/signed`.
fn is_attachment(leaf: &Leaf<'_>) -> bool {
    let mime = mime_type(&leaf.part);
    if mime.starts_with("multipart/") {
        return false;
    }
    if leaf.in_signed
        && (mime == "application/pkcs7-signature" || mime == "application/x-pkcs7-signature")
    {
        return false;
    }
    named_or_attached(&leaf.part) || !mime.starts_with("text/")
}

fn state_of(leaf: &Leaf<'_>, filename: &str, dir: Option<&Path>) -> (AttachmentState, u64) {
    let part = &leaf.part;
    if !part.raw_body().iter().all(u8::is_ascii_whitespace) {
        return (AttachmentState::Inline, part.decoded_len() as u64);
    }
    if let Some(dir) = dir
        && let Ok(Some(path)) = find_external(dir, &leaf.number, filename)
    {
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        return (AttachmentState::External, size);
    }
    match part.headers().get("X-Apple-Content-Length") {
        Some(h) => {
            let size = std::str::from_utf8(h.raw_value())
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            (AttachmentState::NotDownloaded, size)
        }
        None => (AttachmentState::NotExtractable, 0),
    }
}

/// `<dir>/<part>/<filename>`, else the first `<dir>/<sub>/<filename>` in name order, dot dirs
/// skipped. The file found must resolve to a path inside `dir`.
fn find_external(dir: &Path, part: &str, filename: &str) -> Result<Option<PathBuf>, MimeError> {
    let exact = dir.join(part).join(filename);
    if exact.is_file() {
        return contained(dir, &exact).map(Some);
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(None);
    };
    let mut subdirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    for sub in subdirs {
        let candidate = sub.join(filename);
        if candidate.is_file() {
            return contained(dir, &candidate).map(Some);
        }
    }
    Ok(None)
}

fn contained(dir: &Path, file: &Path) -> Result<PathBuf, MimeError> {
    let file = file.canonicalize()?;
    if file.starts_with(dir.canonicalize()?) {
        Ok(file)
    } else {
        Err(MimeError::Unsafe(file))
    }
}

fn truncate(mut text: String, max_chars: usize) -> (String, bool) {
    match text.char_indices().nth(max_chars) {
        Some((at, _)) => {
            text.truncate(at);
            (text, true)
        }
        None => (text, false),
    }
}

/// HTML to plain text: `<script>`/`<style>` dropped with their content, comments dropped, `<br>` and
/// the closing tags of p, div, li, tr and h1–h6 become a newline, other tags vanish; then entities
/// are decoded, runs of spaces and tabs collapse to one space, lines are trimmed, at most one blank
/// line is kept, and the result is trimmed.
pub fn strip_html(html: &str) -> String {
    // ASCII lowercasing keeps every byte offset.
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(off) = html[i..].find('<') {
        let start = i + off;
        out.push_str(&html[i..start]);
        let rest = &lower[start..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            i = comment
                .find("-->")
                .map_or(html.len(), |e| start + 4 + e + 3);
            continue;
        }
        let closing = rest[1..].starts_with('/');
        let name: String = rest[1..]
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        if name.is_empty() && !rest[1..].starts_with(['!', '?', '/']) {
            // A bare `<`, as in "1 < 2".
            out.push('<');
            i = start + 1;
            continue;
        }
        let end = rest.find('>').map_or(html.len(), |e| start + e + 1);
        if !closing && (name == "script" || name == "style") {
            let close = format!("</{name}");
            i = match lower[end..].find(&close) {
                Some(c) => {
                    let c = end + c;
                    lower[c..].find('>').map_or(html.len(), |g| c + g + 1)
                }
                None => html.len(),
            };
            continue;
        }
        let block = matches!(
            name.as_str(),
            "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
        );
        if name == "br" || (closing && block) {
            out.push('\n');
        }
        i = end;
    }
    out.push_str(&html[i..]);
    tidy(&decode_entities(&out))
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        // The longest HTML5 entity name has 31 characters; look no further for the `;`.
        let decoded = tail[1..]
            .bytes()
            .take(33)
            .position(|b| b == b';')
            .and_then(|semi| entity(&tail[1..1 + semi]).map(|c| (c, semi + 2)));
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `&nbsp;` as a plain space; every other named (HTML5 table) or numeric entity through
/// mail-parser, which hands the token back unchanged when it is not one.
fn entity(name: &str) -> Option<char> {
    if name == "nbsp" {
        return Some(' ');
    }
    let token = format!("&{name};");
    let mut out = String::new();
    mail_parser::add_html_token(&mut out, token.as_bytes(), false);
    (out != token).then(|| out.chars().next()).flatten()
}

fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut newlines = 0;
    for (n, line) in s.split('\n').enumerate() {
        if n > 0 {
            newlines += 1;
        }
        let words: Vec<&str> = line
            .split([' ', '\t', '\r'])
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str(if newlines >= 2 { "\n\n" } else { "\n" });
        }
        out.push_str(&words.join(" "));
        newlines = 0;
    }
    out
}
