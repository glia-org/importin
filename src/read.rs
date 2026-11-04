//! Compose one message (index row + emlx + MIME + S/MIME) and search, with a bounded body scan.
//! Decrypted content lives only in the returned `Email`; nothing is cached or written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::Serialize;

use crate::accounts;
use crate::attach::{self, Extracted};
use crate::config::Config;
use crate::emlx::{self, Emlx, EmlxError, Flags};
use crate::envelope::{self, Address, EnvelopeError, Filter, MessageRow, Scope};
use crate::mime::{self, AttachmentInfo, AttachmentState, Body, BodySource, MimeError};
use crate::paths::{Locator, PathError};
use crate::smime::{self, DecryptError, Decryptor, EncryptionInfo, Unwrapped};
use crate::store::{self, StoreError};

const HINT: &str = "narrow with after:, from: or mailbox:";

/// `Send + Sync`, so one `Arc<Context>` serves concurrent requests.
pub struct Context {
    /// Behind a mutex: `rusqlite::Connection` is not `Sync`. Use [`Context::envelope`].
    pub env: Mutex<envelope::Envelope>,
    pub locator: Locator,
    pub decryptor: Box<dyn Decryptor>,
    pub config: Config,
}

impl Context {
    /// The Envelope Index, one query at a time; a poisoned lock is still usable (queries hold no
    /// state between calls).
    pub fn envelope(&self) -> MutexGuard<'_, envelope::Envelope> {
        self.env.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
}

/// The account labels come from `config.accounts_db`; without them `mailbox://` accounts are named
/// by their uuid.
pub fn open(config: Config) -> Result<Context, OpenError> {
    let store = store::locate(&config.mail_root)?;
    let labels = accounts::load(&config.accounts_db).unwrap_or_else(|e| {
        crate::debug!("no account labels: {e}");
        HashMap::new()
    });
    let env = envelope::open(&store, &config.exclude_accounts, &labels)?;
    Ok(Context {
        env: Mutex::new(env),
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
    #[error("{filename} was never fetched from the server; open it in Mail once to download it")]
    NotDownloaded { filename: String },
    #[error("{filename} is declared but not present on disk")]
    NotExtractable { filename: String },
    #[error("cannot decrypt the message: {0}")]
    Decrypt(#[from] DecryptError),
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
    let Loaded {
        row,
        file,
        attachments_dir,
    } = load(ctx, id)?;
    let (outer, unwrapped) = unwrap_outer(ctx, &file.raw)?;
    let headers = outer.headers();
    let dir = Some(attachments_dir.as_path());
    let method = ctx.decryptor.name();

    let (body, attachments, encryption) = match unwrapped {
        Some(Ok(unwrapped)) => {
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
        Some(Err(err)) => {
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
        None => {
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
        }
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

/// The text of one attachment, chosen by exact `filename`, else by `index` (a position in the list
/// `get_email` returns). The message loads as in `get_email`; an enveloped message's attachments
/// come from its decrypted entity, and a decryption failure is an error.
pub fn attachment_text(
    ctx: &Context,
    id: i64,
    filename: Option<&str>,
    index: Option<usize>,
    max_chars: usize,
) -> Result<Extracted, ReadError> {
    let Loaded {
        file,
        attachments_dir,
        ..
    } = load(ctx, id)?;
    let (outer, unwrapped) = unwrap_outer(ctx, &file.raw)?;
    match unwrapped {
        Some(unwrapped) => {
            let unwrapped = unwrapped?;
            let inner = mime::parse(&unwrapped.inner)?;
            extract_one(&inner, None, filename, index, max_chars)
        }
        None => extract_one(&outer, Some(&attachments_dir), filename, index, max_chars),
    }
}

/// A message's index row and emlx file. `get_email`, `attachment_text` and the body scan all load
/// through `load`, so they find and read a message the same way; the first two also unwrap it
/// through `unwrap_outer`.
struct Loaded {
    row: MessageRow,
    file: Emlx,
    attachments_dir: PathBuf,
}

fn load(ctx: &Context, id: i64) -> Result<Loaded, ReadError> {
    let row = ctx.envelope().get(id)?.ok_or(ReadError::NotFound(id))?;
    load_row(ctx, row)
}

/// [`load`] for a row already fetched (the body scan's candidates).
fn load_row(ctx: &Context, row: MessageRow) -> Result<Loaded, ReadError> {
    let located = ctx
        .locator
        .locate(row.id, &row.mailbox_url)?
        .ok_or_else(|| ReadError::FileMissing {
            id: row.id,
            mailbox: row.mailbox.clone(),
        })?;
    let file = emlx::read(&located.emlx)?;
    Ok(Loaded {
        row,
        file,
        attachments_dir: located.attachments_dir,
    })
}

/// The parsed outer message and, when it is enveloped, the outcome of unwrapping it.
type Opened<'a> = (mime::Parsed<'a>, Option<Result<Unwrapped, DecryptError>>);

fn unwrap_outer<'a>(ctx: &Context, raw: &'a [u8]) -> Result<Opened<'a>, ReadError> {
    let outer = mime::parse(raw)?;
    let unwrapped = outer
        .is_enveloped()
        .then(|| smime::unwrap(&outer, ctx.decryptor.as_ref()));
    Ok((outer, unwrapped))
}

fn extract_one(
    parsed: &mime::Parsed<'_>,
    dir: Option<&Path>,
    filename: Option<&str>,
    index: Option<usize>,
    max_chars: usize,
) -> Result<Extracted, ReadError> {
    let list = parsed.attachments(dir);
    let info = filename
        .and_then(|f| list.iter().find(|a| a.filename == f))
        .or_else(|| index.and_then(|i| list.get(i)))
        .ok_or_else(|| {
            MimeError::NoSuchPart(match (filename, index) {
                (Some(f), Some(i)) => format!("filename {f:?} or index {i}"),
                (Some(f), None) => format!("filename {f:?}"),
                (None, Some(i)) => format!("index {i}"),
                (None, None) => "neither filename nor index given".to_string(),
            })
        })?;
    let bytes = parsed.attachment_bytes(info, dir).map_err(|e| {
        let filename = info.filename.clone();
        match e {
            MimeError::Unavailable(AttachmentState::NotDownloaded) => {
                ReadError::NotDownloaded { filename }
            }
            MimeError::Unavailable(AttachmentState::NotExtractable) => {
                ReadError::NotExtractable { filename }
            }
            e => e.into(),
        }
    })?;
    Ok(Extracted {
        state: info.state,
        ..attach::extract(&bytes, &info.filename, &info.mime_type, max_chars)
    })
}

/// Header scopes query the index. `Scope::Body` takes up to `body_scan_max` candidates from the
/// index (newest first, `after` defaulting to `body_scan_days` before `before`, or before now) and
/// searches their bodies for every term, case-insensitively; enveloped messages are not decrypted
/// and never match.
pub fn search(ctx: &Context, f: &Filter) -> Result<SearchResult, ReadError> {
    if f.scope != Scope::Body {
        let hits: Vec<SearchHit> = ctx.envelope().search(f)?.into_iter().map(hit).collect();
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
    let after = f.after.unwrap_or_else(|| {
        let end = f.before.unwrap_or_else(Utc::now);
        end.checked_sub_signed(Duration::days(i64::from(ctx.config.body_scan_days)))
            .unwrap_or(DateTime::<Utc>::MIN_UTC)
    });
    let candidates = ctx.envelope().search(&Filter {
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
                crate::debug!("body scan: message {}: {e}", row.id);
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

/// The full text body; `None` for an enveloped message, which is not decrypted.
fn body_text(ctx: &Context, row: &MessageRow) -> Result<Option<String>, ReadError> {
    let Loaded { file, .. } = load_row(ctx, row.clone())?;
    let parsed = mime::parse(&file.raw)?;
    if parsed.is_enveloped() {
        return Ok(None);
    }
    Ok(Some(parsed.body(usize::MAX, false).text))
}

/// An index row in the `search` result shape (also `get_thread`'s).
pub fn hit(row: MessageRow) -> SearchHit {
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
    mime.eq_ignore_ascii_case("application/pkcs7-mime")
        || mime.eq_ignore_ascii_case("application/x-pkcs7-mime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkcs7_mime_types_match_in_any_case() {
        assert!(is_pkcs7_mime("application/pkcs7-mime"));
        assert!(is_pkcs7_mime("Application/PKCS7-MIME"));
        assert!(is_pkcs7_mime("application/X-PKCS7-Mime"));
        assert!(!is_pkcs7_mime("application/pkcs7-signature"));
    }
}
