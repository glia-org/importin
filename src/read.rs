//! Compose one message (index row + emlx + MIME + S/MIME) and search, with a bounded body scan.
//! Decrypted content lives only in the returned `Email`; nothing is cached or written.

use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::emlx::{self, EmlxError, Flags};
use crate::envelope::{self, Address, EnvelopeError, Filter, MessageRow, Scope};
use crate::mime::{self, AttachmentInfo, Body, BodySource, MimeError};
use crate::paths::{Locator, PathError};
use crate::smime::{self, Decryptor, EncryptionInfo};
use crate::store::{self, StoreError};

const HINT: &str = "narrow with after:, from: or mailbox:";

pub struct Context {
    pub env: envelope::Envelope,
    pub locator: Locator,
    pub decryptor: Box<dyn Decryptor>,
    pub config: Config,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
}

pub fn open(config: Config) -> Result<Context, OpenError> {
    let store = store::locate(&config.mail_root)?;
    let env = envelope::open(&store, &config.exclude_accounts)?;
    Ok(Context {
        env,
        locator: Locator::new(store),
        decryptor: smime::from_config(&config.smime),
        config,
    })
}

/// No `Debug`: the body may be decrypted content.
#[derive(Serialize)]
pub struct Email {
    pub id: i64,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub conversation_id: Option<i64>,
    pub date_sent: Option<DateTime<FixedOffset>>,
    pub date_received: Option<DateTime<FixedOffset>>,
    pub subject: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub mailbox: String,
    pub account: String,
    pub read: bool,
    pub flagged: bool,
    pub body: String,
    pub body_html: Option<String>,
    /// "plain", "html" or "none".
    pub body_source: &'static str,
    pub truncated: bool,
    pub encryption: EncryptionInfo,
    pub attachments: Vec<AttachmentInfo>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("no message with id {0}")]
    NotFound(i64),
    #[error("message {id}: no file in mailbox {mailbox} (not downloaded?)")]
    FileMissing { id: i64, mailbox: String },
    #[error(transparent)]
    Emlx(#[from] EmlxError),
    #[error(transparent)]
    Mime(#[from] MimeError),
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
}

/// The index row as a search hit shows it.
#[derive(Debug, Clone, Serialize)]
pub struct HitRow {
    pub id: i64,
    pub date_received: Option<DateTime<FixedOffset>>,
    pub date_sent: Option<DateTime<FixedOffset>>,
    pub subject: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub mailbox: String,
    pub account: String,
    pub read: bool,
    pub flagged: bool,
    pub conversation_id: Option<i64>,
    pub snippet: Option<String>,
}

/// `encrypted`, `signed`, `has_attachments`: hints from the index flags, no file read.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    #[serde(flatten)]
    pub row: HitRow,
    pub encrypted: bool,
    pub signed: bool,
    pub has_attachments: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    /// Body scan only: candidates parsed.
    pub scanned: Option<usize>,
    /// Body scan only: candidates the index returned (at most `body_scan_max`).
    pub candidates_total: Option<usize>,
    pub truncated: bool,
    pub hint: Option<String>,
    /// Body scan only: candidates whose file could not be located, read or parsed.
    pub skipped: usize,
}

pub fn get_email(ctx: &Context, id: i64, max_chars: usize, html: bool) -> Result<Email, ReadError> {
    let row = ctx.env.get(id)?.ok_or(ReadError::NotFound(id))?;
    let located =
        ctx.locator
            .locate(id, &row.mailbox_url)?
            .ok_or_else(|| ReadError::FileMissing {
                id,
                mailbox: row.mailbox.clone(),
            })?;
    let file = emlx::read(&located.emlx)?;
    let outer = mime::parse(&file.raw)?;
    let headers = outer.headers();
    let dir = Some(located.attachments_dir.as_path());

    let (body, attachments, encryption) = if outer.is_enveloped() {
        let method = ctx.decryptor.name();
        match smime::unwrap(&outer, ctx.decryptor.as_ref()) {
            Ok(unwrapped) => {
                let inner = mime::parse(&unwrapped.inner)?;
                let encryption = EncryptionInfo {
                    encrypted: true,
                    decrypted: true,
                    method: Some(method.into()),
                    signed: unwrapped.signed,
                    signature: if unwrapped.signed {
                        "not verified"
                    } else {
                        "none"
                    }
                    .into(),
                    error: None,
                };
                // The parts of the decrypted entity are inline; the Attachments dir belongs to
                // the outer message's part numbers.
                (
                    inner.body(max_chars, html),
                    inner.attachments(None),
                    encryption,
                )
            }
            Err(err) => {
                let attachments = outer
                    .attachments(dir)
                    .into_iter()
                    .filter(|a| !is_pkcs7_mime(&a.mime_type))
                    .collect();
                let body = Body {
                    text: String::new(),
                    source: BodySource::None,
                    html: None,
                    truncated: false,
                };
                (body, attachments, EncryptionInfo::failed(method, &err))
            }
        }
    } else {
        let encryption = if outer.is_signed() {
            EncryptionInfo {
                signed: true,
                signature: "not verified".into(),
                ..EncryptionInfo::none()
            }
        } else {
            EncryptionInfo::none()
        };
        (
            outer.body(max_chars, html),
            outer.attachments(dir),
            encryption,
        )
    };

    Ok(Email {
        id,
        message_id: headers.message_id,
        in_reply_to: headers.in_reply_to,
        conversation_id: row.conversation_id,
        date_sent: row.date_sent,
        date_received: row.date_received,
        subject: row.subject,
        from: row.from,
        to: row.to,
        cc: row.cc,
        bcc: row.bcc,
        reply_to: headers.reply_to,
        mailbox: row.mailbox,
        account: row.account,
        read: row.read,
        flagged: row.flagged,
        body: body.text,
        body_html: body.html,
        body_source: match body.source {
            BodySource::Plain => "plain",
            BodySource::Html => "html",
            BodySource::None => "none",
        },
        truncated: body.truncated,
        encryption,
        attachments,
    })
}

/// Header scopes query the index. `Scope::Body` takes up to `body_scan_max` candidates from the
/// index (newest first, `after` defaulting to `body_scan_days` ago) and searches their bodies for
/// every term, case-insensitively; enveloped messages are not decrypted and never match.
pub fn search(ctx: &Context, f: &Filter) -> Result<SearchResult, ReadError> {
    if f.scope != Scope::Body {
        let hits: Vec<SearchHit> = ctx.env.search(f)?.into_iter().map(hit).collect();
        return Ok(SearchResult {
            truncated: hits.len() == f.limit,
            hits,
            scanned: None,
            candidates_total: None,
            hint: None,
            skipped: 0,
        });
    }

    let max = ctx.config.body_scan_max;
    let after = f
        .after
        .unwrap_or_else(|| Utc::now() - Duration::days(i64::from(ctx.config.body_scan_days)));
    let candidates = ctx.env.search(&Filter {
        scope: Scope::Body,
        after: Some(after),
        limit: max,
        offset: 0,
        ..f.clone()
    })?;
    let terms: Vec<String> = f
        .terms
        .iter()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();

    let (mut hits, mut matched, mut scanned, mut skipped) = (Vec::new(), 0, 0, 0);
    let mut stopped = false;
    for row in &candidates {
        if hits.len() == f.limit {
            stopped = true;
            break;
        }
        match body_text(ctx, row) {
            Ok(text) => {
                scanned += 1;
                let Some(text) = text.map(|t| t.to_lowercase()) else {
                    continue;
                };
                if terms.iter().all(|t| text.contains(t)) {
                    matched += 1;
                    if matched > f.offset {
                        hits.push(hit(row.clone()));
                    }
                }
            }
            Err(e) => {
                skipped += 1;
                debug(&format!("body scan: message {}: {e}", row.id));
            }
        }
    }
    let truncated = stopped || candidates.len() == max;
    Ok(SearchResult {
        hits,
        scanned: Some(scanned),
        candidates_total: Some(candidates.len()),
        truncated,
        hint: truncated.then(|| HINT.to_string()),
        skipped,
    })
}

/// The full text body; `None` for an enveloped message.
fn body_text(ctx: &Context, row: &MessageRow) -> Result<Option<String>, ReadError> {
    let located = ctx
        .locator
        .locate(row.id, &row.mailbox_url)?
        .ok_or_else(|| ReadError::FileMissing {
            id: row.id,
            mailbox: row.mailbox.clone(),
        })?;
    let file = emlx::read(&located.emlx)?;
    let parsed = mime::parse(&file.raw)?;
    if parsed.is_enveloped() {
        return Ok(None);
    }
    Ok(Some(parsed.body(usize::MAX, false).text))
}

fn hit(row: MessageRow) -> SearchHit {
    let flags = Flags(row.flags as u64);
    SearchHit {
        encrypted: flags.encrypted(),
        signed: flags.signed(),
        has_attachments: flags.attachment_count() > 0,
        row: HitRow {
            id: row.id,
            date_received: row.date_received,
            date_sent: row.date_sent,
            subject: row.subject,
            from: row.from,
            to: row.to,
            cc: row.cc,
            mailbox: row.mailbox,
            account: row.account,
            read: row.read,
            flagged: row.flagged,
            conversation_id: row.conversation_id,
            snippet: row.snippet,
        },
    }
}

fn is_pkcs7_mime(mime: &str) -> bool {
    mime == "application/pkcs7-mime" || mime == "application/x-pkcs7-mime"
}

/// To stderr when `IMPORTIN_LOG` is `debug` or `trace`.
fn debug(msg: &str) {
    let level = std::env::var("IMPORTIN_LOG").unwrap_or_default();
    if level.eq_ignore_ascii_case("debug") || level.eq_ignore_ascii_case("trace") {
        eprintln!("importin: {msg}");
    }
}
