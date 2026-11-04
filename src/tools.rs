//! The MCP server: six read-only tools over stdio. stdout carries MCP messages only; logs go to
//! stderr. The blocking reads run on tokio's blocking pool, sharing one `Arc<Context>`.

use std::fs::File;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveTime, Offset, TimeZone, Utc};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::envelope::{AccountInfo, Filter, MailboxInfo, Scope};
use crate::read::{self, Context, ReadError, SearchHit};

const INSTRUCTIONS: &str = "Read-only access to Apple Mail on this Mac. search finds messages by \
header fields (scope all|subject|from|to|cc|participant) instantly; scope body scans message files \
within a date window and reports when truncated. get_email reads one message, decrypting S/MIME \
when possible. Ids are session-local; message_id is durable.";

const SCOPES: [(&str, Scope); 7] = [
    ("all", Scope::All),
    ("subject", Scope::Subject),
    ("from", Scope::From),
    ("to", Scope::To),
    ("cc", Scope::Cc),
    ("participant", Scope::Participant),
    ("body", Scope::Body),
];

const BODY_NEEDS_QUERY: &str =
    "scope body needs a query; use another scope for header-only filters";

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    /// Words that must all match (case-insensitive substrings) within `scope`. May be empty when
    /// filters do the work, except with scope body.
    query: Option<String>,
    /// One of all (default), subject, from, to, cc, participant, body. all = subject, sender,
    /// recipients and Mail's summary; participant = from, to, cc and bcc. body reads message files
    /// received since `after`, by default the configured window (body_scan_days, default 90 days)
    /// before `before` or now; encrypted bodies are never searched.
    scope: Option<String>,
    /// Received at or after: RFC 3339 (2025-01-31T09:00:00+01:00) or YYYY-MM-DD (local midnight).
    after: Option<String>,
    /// Received before: RFC 3339 or YYYY-MM-DD (local midnight). Must be later than `after`.
    before: Option<String>,
    /// Substring of the mailbox name, e.g. "INBOX" or "Archive".
    mailbox: Option<String>,
    /// Substring of the account, e.g. "user@example.com".
    account: Option<String>,
    /// Substring of the sender's address or name; combines with any scope.
    from: Option<String>,
    /// Only unread messages.
    unread: Option<bool>,
    /// Only flagged messages.
    flagged: Option<bool>,
    /// Maximum hits, 1-200 (default 25).
    limit: Option<u32>,
    /// Hits to skip, for paging (default 0).
    offset: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
struct GetEmailArgs {
    /// Message id from search or get_thread.
    id: i64,
    /// Body characters to return, 500-200000 (default 20000).
    max_chars: Option<u32>,
    /// Also return the raw HTML part as body_html, cut at max_chars (default false).
    html: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
struct GetThreadArgs {
    /// Id of any message in the conversation.
    id: i64,
    /// Maximum messages, the newest kept, 1-200 (default 50).
    limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
struct AttachmentTextArgs {
    /// Message id from search or get_thread.
    id: i64,
    /// Attachment filename exactly as get_email lists it.
    filename: Option<String>,
    /// Attachment index as get_email lists it; used when filename is absent or not found.
    index: Option<u32>,
    /// Text characters to return, 500-100000 (default 20000).
    max_chars: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
struct ListMailboxesArgs {
    /// Only mailboxes whose account contains this substring.
    account: Option<String>,
}

#[derive(Serialize)]
struct Thread {
    messages: Vec<SearchHit>,
    /// Older messages of the conversation were left out.
    truncated: bool,
}

#[derive(Serialize)]
struct Mailboxes {
    mailboxes: Vec<MailboxInfo>,
}

#[derive(Serialize)]
struct Accounts {
    accounts: Vec<AccountInfo>,
}

struct Server {
    ctx: Arc<Context>,
    tool_router: ToolRouter<Server>,
}

/// Moves the MCP channel off fd 1: returns a duplicate of fd 1 (close-on-exec, so child processes
/// do not inherit it) for the transport and points fd 1 at stderr, so a stray print from any
/// dependency lands on stderr instead of corrupting the channel. Call before anything prints.
pub fn guard_stdout() -> std::io::Result<File> {
    let channel = std::io::stdout().as_fd().try_clone_to_owned()?;
    // SAFETY: dup2 on two descriptors this process holds open; it only replaces fd 1.
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(File::from(channel))
}

/// Opens the store, then serves on stdin and `channel` (from [`guard_stdout`]) until the client
/// disconnects.
pub async fn serve(config: Config, channel: File) -> anyhow::Result<()> {
    let ctx = read::open(config)?;
    let server = Server {
        ctx: Arc::new(ctx),
        tool_router: Server::tool_router(),
    };
    crate::info!("serving MCP on stdio");
    let transport = (tokio::io::stdin(), tokio::fs::File::from_std(channel));
    server.serve(transport).await?.waiting().await?;
    Ok(())
}

#[tool_router]
impl Server {
    #[tool(
        description = "Find messages, newest first, with ids for get_email and get_thread. Header \
            scopes query Mail's index instantly; scope body scans message files within a date \
            window and reports scanned, candidates_total and truncated.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let filter = match filter(args) {
            Ok(f) => f,
            Err(msg) => return Ok(error(msg)),
        };
        self.run("search", move |ctx| read::search(ctx, &filter))
            .await
    }

    #[tool(
        description = "Read one message: headers, to/cc/bcc, text body and attachment list. \
            S/MIME-encrypted messages are decrypted when possible; `encryption` reports the outcome.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_email(
        &self,
        Parameters(args): Parameters<GetEmailArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let max_chars = match bounded("max_chars", args.max_chars, 20_000, 500, 200_000) {
            Ok(n) => n as usize,
            Err(msg) => return Ok(error(msg)),
        };
        let html = args.html.unwrap_or(false);
        self.run("get_email", move |ctx| {
            read::get_email(ctx, args.id, max_chars, html)
        })
        .await
    }

    #[tool(
        description = "The newest `limit` messages of the conversation containing `id`, oldest \
            first, in the search hit shape; `truncated` says older ones were left out.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_thread(
        &self,
        Parameters(args): Parameters<GetThreadArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = match bounded("limit", args.limit, 50, 1, 200) {
            Ok(n) => n as usize,
            Err(msg) => return Ok(error(msg)),
        };
        let id = args.id;
        self.run("get_thread", move |ctx| -> Result<Thread, ReadError> {
            let mut rows = ctx.envelope().thread(id, limit + 1)?;
            if rows.is_empty() {
                return Err(ReadError::NotFound(id));
            }
            let truncated = rows.len() > limit;
            if truncated {
                rows.remove(0);
            }
            Ok(Thread {
                messages: rows.into_iter().map(read::hit).collect(),
                truncated,
            })
        })
        .await
    }

    #[tool(
        description = "Text of one attachment, chosen by filename or index as get_email lists \
            them: PDF, Word, Excel/ODS, PowerPoint, HTML and text; other types get a one-line note.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn attachment_text(
        &self,
        Parameters(args): Parameters<AttachmentTextArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let max_chars = match bounded("max_chars", args.max_chars, 20_000, 500, 100_000) {
            Ok(n) => n as usize,
            Err(msg) => return Ok(error(msg)),
        };
        self.run("attachment_text", move |ctx| {
            let index = args.index.map(|i| i as usize);
            read::attachment_text(ctx, args.id, args.filename.as_deref(), index, max_chars)
        })
        .await
    }

    #[tool(
        description = "Mailboxes with message and unread counts, optionally of one account. \
            Excluded accounts are omitted.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_mailboxes(
        &self,
        Parameters(args): Parameters<ListMailboxesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run("list_mailboxes", move |ctx| {
            let mailboxes = ctx.envelope().mailboxes(args.account.as_deref())?;
            Ok::<_, ReadError>(Mailboxes { mailboxes })
        })
        .await
    }

    #[tool(
        description = "Mail accounts with mailbox and message counts. Excluded accounts are omitted.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_accounts(&self) -> Result<CallToolResult, ErrorData> {
        self.run("list_accounts", |ctx| {
            let accounts = ctx.envelope().accounts()?;
            Ok::<_, ReadError>(Accounts { accounts })
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("importin", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

impl Server {
    /// Runs `f` on the blocking pool: `Ok` becomes pretty JSON plus the same structured content,
    /// `Err` a tool error with its message.
    async fn run<T, F>(&self, tool: &'static str, f: F) -> Result<CallToolResult, ErrorData>
    where
        T: Serialize + Send + 'static,
        F: FnOnce(&Context) -> Result<T, ReadError> + Send + 'static,
    {
        let ctx = Arc::clone(&self.ctx);
        let start = Instant::now();
        let out = tokio::task::spawn_blocking(move || f(&ctx)).await;
        let ms = start.elapsed().as_millis();
        match out {
            Ok(Ok(value)) => {
                crate::debug!("{tool}: ok in {ms} ms");
                success(&value)
            }
            Ok(Err(e)) => {
                // The variant only: the message can name a file or a mailbox.
                crate::debug!("{tool}: error in {ms} ms: {}", e.kind());
                Ok(error(e.to_string()))
            }
            // The JoinError's text holds the panic payload, which may be message content.
            Err(_) => {
                crate::warn!("{tool}: task panicked");
                Ok(error(format!(
                    "internal error in {tool}; see the server's stderr"
                )))
            }
        }
    }
}

fn success<T: Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let value =
        serde_json::to_value(value).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    let text = serde_json::to_string_pretty(&value)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(value);
    Ok(result)
}

fn error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg)])
}

fn filter(a: SearchArgs) -> Result<Filter, String> {
    let scope = match a.scope.as_deref() {
        None => Scope::All,
        Some(s) => SCOPES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(s))
            .map(|(_, scope)| *scope)
            .ok_or_else(|| {
                let names: Vec<&str> = SCOPES.iter().map(|(name, _)| *name).collect();
                format!("scope must be one of {}; got {s:?}", names.join(", "))
            })?,
    };
    let terms: Vec<String> = a
        .query
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if scope == Scope::Body && terms.is_empty() {
        return Err(BODY_NEEDS_QUERY.into());
    }
    let after = a
        .after
        .as_deref()
        .map(|s| date("after", s, &Local))
        .transpose()?;
    let before = a
        .before
        .as_deref()
        .map(|s| date("before", s, &Local))
        .transpose()?;
    if let (Some(after), Some(before)) = (after, before)
        && after >= before
    {
        return Err("after must be before before".into());
    }
    Ok(Filter {
        terms,
        scope,
        after,
        before,
        mailbox: a.mailbox,
        account: a.account,
        from: a.from,
        unread: a.unread.unwrap_or(false),
        flagged: a.flagged.unwrap_or(false),
        include_deleted: false,
        limit: bounded("limit", a.limit, 25, 1, 200)? as usize,
        offset: a.offset.unwrap_or(0) as usize,
    })
}

/// RFC 3339, or `YYYY-MM-DD` as midnight in `tz` (the process's local timezone in the tools). When
/// midnight falls in a DST gap, the day starts at the transition: midnight in the offset in force
/// the day before.
fn date<Tz: TimeZone>(field: &str, s: &str, tz: &Tz) -> Result<DateTime<Utc>, String> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| {
            let midnight = d.and_time(NaiveTime::MIN);
            if let Some(t) = tz.from_local_datetime(&midnight).earliest() {
                return Some(t.with_timezone(&Utc));
            }
            // A gap, or an instant outside chrono's range (checked: no overflow panics).
            let day_before = midnight.and_utc().checked_sub_signed(Duration::days(1))?;
            let offset = tz.offset_from_utc_datetime(&day_before.naive_utc()).fix();
            let utc =
                midnight.checked_sub_signed(Duration::seconds(offset.local_minus_utc().into()))?;
            Some(utc.and_utc())
        })
        .ok_or_else(|| {
            format!("{field} must be RFC 3339 (2025-01-31T09:00:00+01:00) or YYYY-MM-DD; got {s:?}")
        })
}

fn bounded(
    name: &str,
    value: Option<u32>,
    default: u32,
    min: u32,
    max: u32,
) -> Result<u32, String> {
    let v = value.unwrap_or(default);
    if (min..=max).contains(&v) {
        Ok(v)
    } else {
        Err(format!("{name} must be between {min} and {max}; got {v}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn plain_date_is_midnight_in_the_timezone() {
        let berlin_winter = FixedOffset::east_opt(3600).unwrap();
        let t = date("after", "2026-01-01", &berlin_winter).unwrap();
        assert_eq!(t.to_rfc3339(), "2025-12-31T23:00:00+00:00");
        let t = date("after", "2026-01-01T09:00:00+01:00", &berlin_winter).unwrap();
        assert_eq!(t.to_rfc3339(), "2026-01-01T08:00:00+00:00");
        assert!(date("after", "2026-02-30", &berlin_winter).is_err());
    }

    #[test]
    fn scope_matches_in_any_case() {
        let args = |scope: &str| -> SearchArgs {
            serde_json::from_value(serde_json::json!({"query": "x", "scope": scope})).unwrap()
        };
        assert_eq!(filter(args("CC")).unwrap().scope, Scope::Cc);
        assert_eq!(
            filter(args("Participant")).unwrap().scope,
            Scope::Participant
        );
        assert!(filter(args("bodies")).is_err());
    }

    #[test]
    fn earliest_date_east_of_utc_is_an_error_not_a_panic() {
        // Local midnight of NaiveDate::MIN lies before the first representable UTC instant.
        let east = FixedOffset::east_opt(3600).unwrap();
        let e = date("after", "-262143-01-01", &east).unwrap_err();
        assert!(e.starts_with("after must be RFC 3339"), "{e}");
        assert!(
            date(
                "before",
                "-262143-01-01",
                &FixedOffset::west_opt(3600).unwrap()
            )
            .is_ok()
        );
    }
}
