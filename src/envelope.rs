//! The Envelope Index: open it read-only (snapshot copy as fallback), resolve the schema at runtime,
//! detect the timestamp epoch, and run the metadata queries (search, get, thread, mailboxes,
//! accounts). Nothing is ever written.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, Row, params_from_iter};
use serde::Serialize;

use crate::accounts::AccountLabel;
use crate::store::Store;

const COCOA_OFFSET: i64 = 978_307_200;

const TABLES: [&str; 6] = [
    "messages",
    "subjects",
    "addresses",
    "mailboxes",
    "recipients",
    "summaries",
];

/// `table.column`s without which the server cannot run.
const REQUIRED: [&str; 8] = [
    "messages.ROWID",
    "messages.date_received",
    "messages.subject",
    "messages.sender",
    "messages.mailbox",
    "subjects.subject",
    "addresses.address",
    "mailboxes.url",
];

/// `table.column`s used when present; each also names the SQL expression that reads it.
const OPTIONAL: [&str; 15] = [
    "messages.date_sent",
    "messages.subject_prefix",
    "messages.read",
    "messages.flagged",
    "messages.deleted",
    "messages.conversation_id",
    "messages.flags",
    "messages.size",
    "messages.summary",
    "addresses.comment",
    "recipients.message",
    "recipients.address",
    "recipients.type",
    "recipients.position",
    "summaries.summary",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenMode {
    ReadOnly,
    /// Path of the private copy being read.
    Snapshot(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub optional_found: Vec<&'static str>,
    pub optional_missing: Vec<&'static str>,
    pub has_recipients: bool,
    pub has_summaries: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("cannot open the Envelope Index: {0}")]
    Open(#[source] rusqlite::Error),
    /// `dump` holds the `CREATE TABLE` statements of every table, for stderr.
    #[error("unknown Envelope Index schema: missing {}", .missing.join(", "))]
    Schema { missing: Vec<String>, dump: String },
    #[error("Envelope Index query failed: {0}")]
    Query(#[from] rusqlite::Error),
    #[error("exclude_accounts pattern {pattern:?}: {source}")]
    Pattern {
        pattern: String,
        source: glob::PatternError,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Address {
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub date_received: Option<DateTime<FixedOffset>>,
    pub date_sent: Option<DateTime<FixedOffset>>,
    pub subject: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub mailbox_url: String,
    pub mailbox: String,
    pub account: String,
    pub read: bool,
    pub flagged: bool,
    pub deleted: bool,
    pub flags: i64,
    pub conversation_id: Option<i64>,
    pub snippet: Option<String>,
    pub size: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Subject,
    From,
    To,
    Cc,
    Participant,
    Body,
}

#[derive(Debug, Clone)]
pub struct Filter {
    pub terms: Vec<String>,
    pub scope: Scope,
    pub after: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    /// Case-insensitive substring of the mailbox display name.
    pub mailbox: Option<String>,
    /// Case-insensitive substring of the account identifier.
    pub account: Option<String>,
    /// Substring of the sender's address or name, with any scope (`Body` too).
    pub from: Option<String>,
    pub unread: bool,
    pub flagged: bool,
    pub include_deleted: bool,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailboxInfo {
    pub name: String,
    pub url: String,
    pub account: String,
    pub messages: i64,
    pub unread: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountInfo {
    pub identifier: String,
    /// The account's name in the Accounts database (a `mailbox://` URL with a label only).
    pub label: Option<String>,
    /// The account type there, e.g. "IMAP" or "Google".
    pub kind: Option<String>,
    /// The account uuid of a `mailbox://` URL.
    pub uuid: Option<String>,
    pub user: String,
    pub host: String,
    pub scheme: String,
    pub mailboxes: i64,
    pub messages: i64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Counts {
    pub messages: i64,
    pub mailboxes: i64,
    pub accounts: i64,
    pub excluded_messages: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlParts {
    pub scheme: String,
    pub authority: String,
    pub user: String,
    pub host: String,
    pub account_uuid: Option<String>,
    /// Percent-decoded path, e.g. `Archive/2025`.
    pub mailbox_name: String,
    /// `user` if it holds an `@`, else `user@host`, else the account uuid, else the authority.
    pub identifier: String,
}

pub struct Envelope {
    conn: Connection,
    schema: Schema,
    epoch_offset: i64,
    /// Lowercase `table.column` of every column of `TABLES`, plus `table.rowid`.
    columns: HashSet<String>,
    /// `exclude_accounts`, lowercased; matched against the mailboxes on every query.
    patterns: Vec<glob::Pattern>,
    /// Account labels by uppercased uuid, for `mailbox://<uuid>/…` URLs.
    labels: HashMap<String, AccountLabel>,
    /// `SELECT … FROM messages … WHERE 1`; callers append the exclusion and `AND …`.
    select: String,
    recipients: Option<String>,
    pub mode: OpenMode,
    // Declared after `conn`, so the copy outlives the connection reading it.
    _snapshot: Option<tempfile::TempDir>,
}

/// `labels` (from [`crate::accounts::load`], may be empty) name the accounts of `mailbox://` URLs.
pub fn open(
    store: &Store,
    exclude_accounts: &[String],
    labels: &HashMap<String, AccountLabel>,
) -> Result<Envelope, EnvelopeError> {
    let patterns = exclude_accounts
        .iter()
        .map(|p| {
            glob::Pattern::new(&p.to_lowercase()).map_err(|source| EnvelopeError::Pattern {
                pattern: p.clone(),
                source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let index = &store.envelope_index;
    let (conn, mode, snapshot_dir) = match connect(index) {
        Ok(conn) => (conn, OpenMode::ReadOnly, None),
        Err(e) => {
            crate::warn!("read-only open failed ({e}); reading a snapshot copy");
            let (dir, copy) = snapshot(index).map_err(|io| {
                crate::warn!("snapshot copy failed: {io}");
                EnvelopeError::Open(e)
            })?;
            let conn = connect(&copy).map_err(EnvelopeError::Open)?;
            (conn, OpenMode::Snapshot(copy), Some(dir))
        }
    };

    let columns = columns(&conn).map_err(EnvelopeError::Open)?;
    let missing: Vec<String> = REQUIRED
        .iter()
        .filter(|c| !columns.contains(&c.to_ascii_lowercase()))
        .map(|c| c.to_string())
        .collect();
    if !missing.is_empty() {
        let dump = schema_dump(&conn).map_err(EnvelopeError::Open)?;
        return Err(EnvelopeError::Schema { missing, dump });
    }
    let has = |c: &str| columns.contains(c);
    let (optional_found, optional_missing) = OPTIONAL.iter().copied().partition(|c| has(c));
    let schema = Schema {
        optional_found,
        optional_missing,
        has_recipients: [
            "recipients.message",
            "recipients.address",
            "recipients.type",
            "recipients.position",
        ]
        .iter()
        .all(|c| has(c)),
        has_summaries: has("summaries.summary") && has("messages.summary"),
    };
    let epoch_offset = detect_epoch(&conn).map_err(EnvelopeError::Open)?;

    let col = |c: &'static str| if has(c) { c } else { "NULL" };
    let comment = |alias: &str| {
        if has("addresses.comment") {
            format!("{alias}.comment")
        } else {
            "NULL".into()
        }
    };
    let (summary, summaries_join) = if schema.has_summaries {
        (
            "summaries.summary",
            " LEFT JOIN summaries ON messages.summary = summaries.ROWID",
        )
    } else {
        ("NULL", "")
    };
    let select = format!(
        "SELECT messages.ROWID, messages.date_received, {}, {}, subjects.subject, sa.address, {}, \
         mailboxes.url, {}, {}, {}, {}, {}, {summary}, {} \
         FROM messages \
         LEFT JOIN subjects ON subjects.ROWID = messages.subject \
         LEFT JOIN addresses sa ON sa.ROWID = messages.sender \
         LEFT JOIN mailboxes ON mailboxes.ROWID = messages.mailbox{summaries_join} \
         WHERE 1",
        col("messages.date_sent"),
        col("messages.subject_prefix"),
        comment("sa"),
        col("messages.read"),
        col("messages.flagged"),
        col("messages.deleted"),
        col("messages.flags"),
        col("messages.conversation_id"),
        col("messages.size"),
    );
    let recipients = schema.has_recipients.then(|| {
        format!(
            "SELECT r.type, a.address, {} FROM recipients r LEFT JOIN addresses a ON a.ROWID = r.address \
             WHERE r.message = ? ORDER BY r.type, r.position",
            comment("a")
        )
    });

    Ok(Envelope {
        conn,
        schema,
        epoch_offset,
        columns,
        patterns,
        labels: labels.clone(),
        select,
        recipients,
        mode,
        _snapshot: snapshot_dir,
    })
}

/// `file:<path>?mode=ro` only: no immutable flag, which would hide un-checkpointed WAL content.
/// Also opens the Accounts database.
pub(crate) fn connect(path: &Path) -> rusqlite::Result<Connection> {
    // In a URI, `?` and `#` end the path and `%` starts an escape.
    let p = path
        .to_string_lossy()
        .replace('%', "%25")
        .replace('?', "%3F")
        .replace('#', "%23");
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(format!("file:{p}?mode=ro"), flags)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    // SQLite opens lazily: an unreadable `-wal`/`-shm` only shows on the first read.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
    Ok(conn)
}

/// Copies the index and those of its `-wal`/`-shm` that exist into a private temp dir. An unreadable
/// `-shm` is left out: it is only the WAL's index, and SQLite rebuilds it from the `-wal`.
fn snapshot(index: &Path) -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let copy = dir
        .path()
        .join(index.file_name().unwrap_or("Envelope Index".as_ref()));
    std::fs::copy(index, &copy)?;
    for suffix in ["-wal", "-shm"] {
        let with = |p: &Path| {
            let mut s = p.as_os_str().to_owned();
            s.push(suffix);
            PathBuf::from(s)
        };
        if !with(index).exists() {
            continue;
        }
        if let Err(e) = std::fs::copy(with(index), with(&copy)) {
            if suffix != "-shm" {
                return Err(e);
            }
            crate::warn!("snapshot without -shm: {e}");
        }
    }
    Ok((dir, copy))
}

fn columns(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut set = HashSet::new();
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?)")?;
    for table in TABLES {
        let names = stmt
            .query_map([table], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !names.is_empty() {
            // Mail's tables are rowid tables: ROWID exists even when not declared.
            set.insert(format!("{table}.rowid"));
        }
        set.extend(
            names
                .into_iter()
                .map(|n| format!("{table}.{}", n.to_ascii_lowercase())),
        );
    }
    Ok(set)
}

fn schema_dump(conn: &Connection) -> rusqlite::Result<String> {
    let mut stmt = conn.prepare(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND sql IS NOT NULL ORDER BY name",
    )?;
    let sql = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sql.join(";\n"))
}

/// `MAX(date_received)` ≥ 1e9 means Unix seconds, else Cocoa seconds (since 2001-01-01).
fn detect_epoch(conn: &Connection) -> rusqlite::Result<i64> {
    let max: Option<f64> =
        conn.query_row("SELECT MAX(date_received) FROM messages", [], |r| r.get(0))?;
    Ok(match max {
        Some(v) if v < 1_000_000_000.0 => COCOA_OFFSET,
        _ => 0,
    })
}

fn mailbox_urls(conn: &Connection) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT ROWID, url FROM mailboxes ORDER BY ROWID")?;
    stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        ))
    })?
    .collect()
}

/// Any (lowercased) pattern matching the lowercased user, host, `user@host`, account uuid, or the
/// label's username, description or kind excludes the mailbox. Empty components are never matched,
/// nor `user@host` unless both are set.
fn is_excluded(u: &UrlParts, label: Option<&AccountLabel>, patterns: &[glob::Pattern]) -> bool {
    let mut names = vec![u.user.clone(), u.host.clone()];
    if !u.user.is_empty() && !u.host.is_empty() {
        names.push(format!("{}@{}", u.user, u.host));
    }
    names.extend(u.account_uuid.clone());
    if let Some(l) = label {
        names.extend(
            [&l.username, &l.description, &l.kind]
                .into_iter()
                .flatten()
                .cloned(),
        );
    }
    names
        .iter()
        .filter(|n| !n.is_empty())
        .map(|n| n.to_lowercase())
        .any(|n| patterns.iter().any(|p| p.matches(&n)))
}

/// `%term%` with `%`, `_` and `\` escaped for `LIKE … ESCAPE '\'`.
fn like_pattern(term: &str) -> String {
    let mut s = String::from("%");
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

impl Envelope {
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// 0 for Unix timestamps, 978 307 200 for Cocoa ones.
    pub fn epoch_offset(&self) -> i64 {
        self.epoch_offset
    }

    /// Every term must match within `f.scope`; newest first. `Scope::Body` ignores the terms and
    /// applies only the other filters (the caller scans the bodies).
    pub fn search(&self, f: &Filter) -> Result<Vec<MessageRow>, EnvelopeError> {
        let mut sql = self.select.clone() + &self.exclusion()?;
        let mut p = Vec::new();
        if !f.include_deleted {
            sql += &format!(" AND {} = 0", self.or_zero("messages.deleted"));
        }
        if f.mailbox.is_some() || f.account.is_some() {
            let mailbox = f.mailbox.as_deref().map(str::to_lowercase);
            let account = f.account.as_deref().map(str::to_lowercase);
            let ids: Vec<String> = self
                .scan()?
                .into_iter()
                .filter(|b| {
                    !b.excluded
                        && mailbox
                            .as_ref()
                            .is_none_or(|m| b.parts.mailbox_name.to_lowercase().contains(m))
                        && account
                            .as_ref()
                            .is_none_or(|a| b.parts.identifier.to_lowercase().contains(a))
                })
                .map(|b| b.id.to_string())
                .collect();
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            sql += &format!(" AND messages.mailbox IN ({})", ids.join(", "));
        }
        if let Some(t) = f.after {
            sql += " AND messages.date_received >= ?";
            p.push(Value::Integer(t.timestamp() - self.epoch_offset));
        }
        if let Some(t) = f.before {
            sql += " AND messages.date_received < ?";
            p.push(Value::Integer(t.timestamp() - self.epoch_offset));
        }
        if f.unread {
            sql += &format!(" AND {} = 0", self.or_zero("messages.read"));
        }
        if f.flagged {
            sql += &format!(" AND {} != 0", self.or_zero("messages.flagged"));
        }
        if let Some(from) = f.from.as_deref().filter(|s| !s.is_empty()) {
            let clause = self.address_like("sa", &like_pattern(from), &mut p);
            sql += " AND ";
            sql += &clause;
        }
        if f.scope != Scope::Body {
            for term in f.terms.iter().filter(|t| !t.is_empty()) {
                let clause = self.term(f.scope, &like_pattern(term), &mut p);
                sql += " AND ";
                sql += &clause;
            }
        }
        sql += " ORDER BY messages.date_received DESC, messages.ROWID DESC LIMIT ? OFFSET ?";
        p.push(Value::Integer(f.limit as i64));
        p.push(Value::Integer(f.offset as i64));
        self.rows(&sql, p)
    }

    /// The row, deleted or not; `None` if absent or in an excluded account.
    pub fn get(&self, id: i64) -> Result<Option<MessageRow>, EnvelopeError> {
        self.get_with(id, &self.exclusion()?)
    }

    fn get_with(&self, id: i64, exclusion: &str) -> Result<Option<MessageRow>, EnvelopeError> {
        let sql = format!("{}{exclusion} AND messages.ROWID = ?", self.select);
        Ok(self
            .rows(&sql, vec![Value::Integer(id)])?
            .into_iter()
            .next())
    }

    /// Non-deleted rows sharing the message's `conversation_id`: the newest `limit`, oldest first;
    /// just the message when it has none (or the schema lacks the column); empty if the message is
    /// invisible.
    pub fn thread(&self, id: i64, limit: usize) -> Result<Vec<MessageRow>, EnvelopeError> {
        let exclusion = self.exclusion()?;
        let Some(m) = self.get_with(id, &exclusion)? else {
            return Ok(Vec::new());
        };
        let Some(conv) = m.conversation_id else {
            return Ok(vec![m]);
        };
        let sql = format!(
            "{}{exclusion} AND messages.conversation_id = ? AND {} = 0 \
             ORDER BY messages.date_received DESC, messages.ROWID DESC LIMIT ?",
            self.select,
            self.or_zero("messages.deleted")
        );
        let mut rows = self.rows(
            &sql,
            vec![Value::Integer(conv), Value::Integer(limit as i64)],
        )?;
        rows.reverse();
        Ok(rows)
    }

    /// Mailboxes of visible accounts, optionally only those whose account identifier contains
    /// `account` (case-insensitive); counts are of non-deleted rows.
    pub fn mailboxes(&self, account: Option<&str>) -> Result<Vec<MailboxInfo>, EnvelopeError> {
        let counts = self.mailbox_counts()?;
        let stored_unread = self.stored_unread()?;
        let account = account.map(str::to_lowercase);
        let mut out = Vec::new();
        for b in self.scan()? {
            if b.excluded
                || account
                    .as_ref()
                    .is_some_and(|a| !b.parts.identifier.to_lowercase().contains(a))
            {
                continue;
            }
            let (messages, unread) = counts.get(&b.id).copied().unwrap_or((0, 0));
            let unread = match &stored_unread {
                Some(m) => m.get(&b.id).copied().unwrap_or(0),
                None => unread,
            };
            out.push(MailboxInfo {
                name: b.parts.mailbox_name,
                url: b.url,
                account: b.parts.identifier,
                messages,
                unread,
            });
        }
        Ok(out)
    }

    pub fn accounts(&self) -> Result<Vec<AccountInfo>, EnvelopeError> {
        let counts = self.mailbox_counts()?;
        Ok(accounts_from(&counts, self.scan()?))
    }

    pub fn count(&self) -> Result<Counts, EnvelopeError> {
        let counts = self.mailbox_counts()?;
        let boxes = self.scan()?;
        let excluded: HashSet<i64> = boxes.iter().filter(|b| b.excluded).map(|b| b.id).collect();
        let (mut messages, mut excluded_messages) = (0, 0);
        for (id, (n, _)) in &counts {
            if excluded.contains(id) {
                excluded_messages += n;
            } else {
                messages += n;
            }
        }
        let accounts = accounts_from(&counts, boxes);
        Ok(Counts {
            messages,
            mailboxes: accounts.iter().map(|a| a.mailboxes).sum(),
            accounts: accounts.len() as i64,
            excluded_messages,
        })
    }

    /// ROWIDs with flag bit 3 (encrypted), not deleted, not excluded, newest first.
    pub fn newest_encrypted_ids(&self, n: usize) -> Result<Vec<i64>, EnvelopeError> {
        if !self.has("messages.flags") {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT messages.ROWID FROM messages WHERE (messages.flags & 8) != 0 AND {} = 0{} \
             ORDER BY messages.date_received DESC, messages.ROWID DESC LIMIT ?",
            self.or_zero("messages.deleted"),
            self.exclusion()?
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let ids = stmt
            .query_map([n as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    fn has(&self, column: &str) -> bool {
        self.columns.contains(column)
    }

    fn or_zero(&self, column: &str) -> String {
        if self.has(column) {
            format!("COALESCE({column}, 0)")
        } else {
            "0".into()
        }
    }

    /// The SQL condition for one term in `scope`; pushes its parameters onto `p` in order.
    fn term(&self, scope: Scope, pat: &str, p: &mut Vec<Value>) -> String {
        match scope {
            Scope::Subject => self.subject_like(pat, p),
            Scope::From => self.address_like("sa", pat, p),
            Scope::To => self.recipient_like(Some(0), pat, p),
            Scope::Cc => self.recipient_like(Some(1), pat, p),
            Scope::Participant => {
                let sender = self.address_like("sa", pat, p);
                let recipient = self.recipient_like(None, pat, p);
                format!("({sender} OR {recipient})")
            }
            Scope::All => {
                let mut any = vec![
                    self.subject_like(pat, p),
                    self.address_like("sa", pat, p),
                    self.recipient_like(None, pat, p),
                ];
                if self.schema.has_summaries {
                    any.push(like("summaries.summary", pat, p));
                }
                format!("({})", any.join(" OR "))
            }
            // Body terms are matched by the caller against the message text.
            Scope::Body => "1".into(),
        }
    }

    fn subject_like(&self, pat: &str, p: &mut Vec<Value>) -> String {
        let prefix = if self.has("messages.subject_prefix") {
            "COALESCE(messages.subject_prefix, '') || "
        } else {
            ""
        };
        like(&format!("({prefix}COALESCE(subjects.subject, ''))"), pat, p)
    }

    fn address_like(&self, alias: &str, pat: &str, p: &mut Vec<Value>) -> String {
        let address = like(&format!("{alias}.address"), pat, p);
        if !self.has("addresses.comment") {
            return address;
        }
        let comment = like(&format!("{alias}.comment"), pat, p);
        format!("({address} OR {comment})")
    }

    /// `kind`: 0 To, 1 Cc, 2 Bcc, `None` any.
    fn recipient_like(&self, kind: Option<i64>, pat: &str, p: &mut Vec<Value>) -> String {
        if !self.schema.has_recipients {
            return "0".into();
        }
        let kind = kind
            .map(|k| format!(" AND r.type = {k}"))
            .unwrap_or_default();
        format!(
            "EXISTS (SELECT 1 FROM recipients r JOIN addresses a ON a.ROWID = r.address \
             WHERE r.message = messages.ROWID{kind} AND {})",
            self.address_like("a", pat, p)
        )
    }

    fn rows(&self, sql: &str, p: Vec<Value>) -> Result<Vec<MessageRow>, EnvelopeError> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(params_from_iter(p))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let mut m = self.message(row)?;
            self.add_recipients(&mut m)?;
            out.push(m);
        }
        Ok(out)
    }

    /// One row of `self.select`.
    fn message(&self, r: &Row) -> rusqlite::Result<MessageRow> {
        let text = |i: usize| -> rusqlite::Result<String> {
            Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
        };
        let int =
            |i: usize| -> rusqlite::Result<i64> { Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0)) };
        let url = text(7)?;
        let (u, _) = self.parts(&url);
        Ok(MessageRow {
            id: r.get(0)?,
            date_received: self.date(r.get(1)?),
            date_sent: self.date(r.get(2)?),
            subject: text(3)? + &text(4)?,
            from: Address {
                name: text(6)?,
                address: text(5)?,
            },
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            mailbox: u.mailbox_name,
            account: u.identifier,
            mailbox_url: url,
            read: int(8)? != 0,
            flagged: int(9)? != 0,
            deleted: int(10)? != 0,
            flags: int(11)?,
            conversation_id: r.get(12)?,
            snippet: r.get(13)?,
            size: r.get(14)?,
        })
    }

    fn add_recipients(&self, m: &mut MessageRow) -> rusqlite::Result<()> {
        let Some(sql) = &self.recipients else {
            return Ok(());
        };
        let mut stmt = self.conn.prepare_cached(sql)?;
        let mut rows = stmt.query([m.id])?;
        while let Some(r) = rows.next()? {
            let a = Address {
                address: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            };
            match r.get::<_, Option<i64>>(0)? {
                Some(0) => m.to.push(a),
                Some(1) => m.cc.push(a),
                Some(2) => m.bcc.push(a),
                _ => {}
            }
        }
        Ok(())
    }

    /// Index seconds (REAL or INTEGER) to a UTC timestamp.
    fn date(&self, v: Option<f64>) -> Option<DateTime<FixedOffset>> {
        let t = v? + self.epoch_offset as f64;
        if !t.is_finite() {
            return None;
        }
        let secs = t.floor();
        let nanos = (((t - secs) * 1e9) as u32).min(999_999_999);
        DateTime::from_timestamp(secs as i64, nanos).map(|d| d.fixed_offset())
    }

    /// [`parse_mailbox_url`] with the account's label, if a `mailbox://` URL has one; its identifier
    /// is then the label's username if that holds an `@`, else its description, else the uuid.
    fn parts(&self, url: &str) -> (UrlParts, Option<&AccountLabel>) {
        let mut u = parse_mailbox_url(url);
        let label = u
            .account_uuid
            .as_ref()
            .and_then(|uuid| self.labels.get(&uuid.to_ascii_uppercase()));
        let name = label.and_then(|l| {
            l.username
                .as_ref()
                .filter(|n| n.contains('@'))
                .or(l.description.as_ref())
        });
        if let Some(name) = name {
            u.identifier = name.clone();
        }
        (u, label)
    }

    /// The mailboxes as they are now, each judged against the exclude patterns. Read on every
    /// query, so a mailbox created after open is covered without a restart.
    fn scan(&self) -> rusqlite::Result<Vec<Mailbox>> {
        Ok(mailbox_urls(&self.conn)?
            .into_iter()
            .map(|(id, url)| {
                let (parts, label) = self.parts(&url);
                let label = label.cloned();
                let excluded = is_excluded(&parts, label.as_ref(), &self.patterns);
                Mailbox {
                    id,
                    url,
                    parts,
                    label,
                    excluded,
                }
            })
            .collect())
    }

    /// `" AND messages.mailbox NOT IN (…)"` for the mailboxes excluded now, or empty.
    fn exclusion(&self) -> rusqlite::Result<String> {
        if self.patterns.is_empty() {
            return Ok(String::new());
        }
        let ids: Vec<String> = self
            .scan()?
            .into_iter()
            .filter(|b| b.excluded)
            .map(|b| b.id.to_string())
            .collect();
        if ids.is_empty() {
            return Ok(String::new());
        }
        Ok(format!(" AND messages.mailbox NOT IN ({})", ids.join(", ")))
    }

    /// Non-deleted `(messages, unread)` per mailbox ROWID, counted from `messages`.
    fn mailbox_counts(&self) -> rusqlite::Result<HashMap<i64, (i64, i64)>> {
        let sql = format!(
            "SELECT messages.mailbox, COUNT(*), SUM({} = 0) FROM messages WHERE {} = 0 \
             GROUP BY messages.mailbox",
            self.or_zero("messages.read"),
            self.or_zero("messages.deleted")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get(1)?, r.get(2)?))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            if let (Some(mailbox), n, unread) = row? {
                map.insert(mailbox, (n, unread));
            }
        }
        Ok(map)
    }

    /// Without `messages.read`: `mailboxes.unread_count` per ROWID (empty if that is absent too).
    /// With it: `None`, the count from `messages` applies.
    fn stored_unread(&self) -> rusqlite::Result<Option<HashMap<i64, i64>>> {
        if self.has("messages.read") {
            return Ok(None);
        }
        if !self.has("mailboxes.unread_count") {
            return Ok(Some(HashMap::new()));
        }
        let mut stmt = self
            .conn
            .prepare("SELECT ROWID, unread_count FROM mailboxes")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0)))
        })?;
        rows.collect::<rusqlite::Result<_>>().map(Some)
    }
}

struct Mailbox {
    id: i64,
    url: String,
    parts: UrlParts,
    label: Option<AccountLabel>,
    excluded: bool,
}

/// Visible accounts (one per identifier and uuid) in first-seen mailbox order, with their mailbox
/// and non-deleted message counts.
fn accounts_from(counts: &HashMap<i64, (i64, i64)>, boxes: Vec<Mailbox>) -> Vec<AccountInfo> {
    let mut out: Vec<AccountInfo> = Vec::new();
    for b in boxes.into_iter().filter(|b| !b.excluded) {
        let n = counts.get(&b.id).map_or(0, |c| c.0);
        let u = b.parts;
        match out
            .iter_mut()
            .find(|a| a.identifier == u.identifier && a.uuid == u.account_uuid)
        {
            Some(a) => {
                a.mailboxes += 1;
                a.messages += n;
            }
            None => out.push(AccountInfo {
                identifier: u.identifier,
                label: b.label.as_ref().and_then(|l| l.description.clone()),
                kind: b.label.and_then(|l| l.kind),
                uuid: u.account_uuid,
                user: u.user,
                host: u.host,
                scheme: u.scheme,
                mailboxes: 1,
                messages: n,
            }),
        }
    }
    out
}

fn like(expr: &str, pat: &str, p: &mut Vec<Value>) -> String {
    p.push(Value::Text(pat.to_owned()));
    format!("{expr} LIKE ? ESCAPE '\\'")
}

/// `imap://paul%40example.test@mail.example.test/INBOX` → user `paul@example.test`, host
/// `mail.example.test`; `mailbox://<uuid>/INBOX` → `account_uuid` `<uuid>`, empty user and host.
pub fn parse_mailbox_url(url: &str) -> UrlParts {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (user, host) = match authority.rsplit_once('@') {
        Some((u, h)) => (pct_decode(u), h.to_owned()),
        None => (String::new(), authority.to_owned()),
    };
    // A bare uuid authority names the account (its directory under V<n>), not a host.
    let (host, account_uuid) = if user.is_empty() && is_uuid(&host) {
        (String::new(), Some(host))
    } else {
        (host, None)
    };
    let identifier = if user.contains('@') {
        user.clone()
    } else if !user.is_empty() && !host.is_empty() {
        format!("{user}@{host}")
    } else {
        account_uuid.clone().unwrap_or_else(|| authority.to_owned())
    };
    UrlParts {
        scheme: scheme.to_owned(),
        authority: authority.to_owned(),
        user,
        host,
        account_uuid,
        mailbox_name: pct_decode(path),
        identifier,
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |i: usize| b.get(i).and_then(|&c| (c as char).to_digit(16));
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(hi), Some(lo)) = (hex(i + 1), hex(i + 2))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}
