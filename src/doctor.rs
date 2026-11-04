//! `importin doctor`: Full Disk Access, the store and its schema, path resolution and the flag bits on
//! a sample, and the S/MIME decrypt probe (DESIGN §9). The report holds counts, booleans, timings,
//! account identifiers and error text, never message content or subjects; `--anon` also drops the
//! account identifiers, exclusion patterns and mail root, and scrubs paths and addresses from the text.

use std::fmt::Display;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, Local};
use serde::Serialize;

use crate::config::{Config, DecryptorKind};
use crate::emlx::{self, Flags};
use crate::envelope::{
    AccountInfo, Counts, Filter, MessageRow, OpenMode, Scope, parse_mailbox_url,
};
use crate::mime;
use crate::paths::Resolution;
use crate::read::{self, Context, ReadError};
use crate::smime::{self, DecryptError, SecurityCms};
use crate::store::{self, StoreError};

/// Long enough for a click in the keychain's access dialog.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the literal mail root becomes in `--anon` text.
const ROOT: &str = "<root>";

#[derive(Debug, Serialize)]
pub struct SchemaReport {
    pub optional_found: Vec<&'static str>,
    pub optional_missing: Vec<&'static str>,
    pub has_recipients: bool,
    pub has_summaries: bool,
}

/// How the newest `sampled` messages' files were found.
#[derive(Debug, Serialize)]
pub struct PathSample {
    pub sampled: usize,
    pub formula: usize,
    pub walk: usize,
    pub missing: usize,
}

/// The emlx flag bit 3 against the content type, on the path sample; files that cannot be read or
/// parsed count in neither column.
#[derive(Debug, Serialize)]
pub struct FlagCheck {
    pub sampled: usize,
    pub encrypted_flag_and_type_agree: usize,
    pub disagree: usize,
}

/// One decryption of the newest enveloped message; the content is discarded unread.
#[derive(Debug, Serialize)]
pub struct DecryptProbe {
    /// "security" or "openssl".
    pub method: &'static str,
    pub tried: bool,
    pub ok: bool,
    pub status: Option<i32>,
    pub stderr: String,
    pub ms: u128,
}

#[derive(Debug, Default, Serialize)]
pub struct Timings {
    pub open: Option<u128>,
    pub search_header: Option<u128>,
    pub get_email: Option<u128>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: String,
    /// Built with `--anon`.
    pub anon: bool,
    pub mail_root: String,
    pub version_dir: Option<String>,
    /// "ok" | "missing or denied" | "undetermined"
    pub full_disk_access: &'static str,
    /// "read-only" | "snapshot"
    pub envelope_mode: Option<&'static str>,
    pub schema: Option<SchemaReport>,
    /// "unix" | "cocoa"
    pub epoch: Option<&'static str>,
    pub counts: Option<Counts>,
    pub excluded_accounts: Vec<String>,
    /// Emptied under `--anon`.
    pub accounts: Vec<AccountInfo>,
    pub mail_running: bool,
    pub envelope_mtime: Option<DateTime<FixedOffset>>,
    /// "imap-user-host" | "mailbox-uuid" | "mixed"
    pub url_form: Option<&'static str>,
    pub path_sample: Option<PathSample>,
    pub flag_check: Option<FlagCheck>,
    /// Lines of `security find-identity -v -p smime` naming an identity.
    pub smime_identities: Option<u32>,
    pub decrypt_probe: Option<DecryptProbe>,
    pub timings_ms: Timings,
    pub errors: Vec<String>,
}

pub struct Options {
    pub anon: bool,
    pub json: bool,
    /// Messages in the path sample and flag check.
    pub sample: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            anon: false,
            json: false,
            sample: 20,
        }
    }
}

pub fn run(config: &Config, opts: &Options) -> Report {
    let mut r = Report {
        version: env!("CARGO_PKG_VERSION").into(),
        anon: opts.anon,
        mail_root: config.mail_root.display().to_string(),
        version_dir: None,
        full_disk_access: "undetermined",
        envelope_mode: None,
        schema: None,
        epoch: None,
        counts: None,
        excluded_accounts: config.exclude_accounts.clone(),
        accounts: Vec::new(),
        mail_running: store::mail_running(),
        envelope_mtime: None,
        url_form: None,
        path_sample: None,
        flag_check: None,
        smime_identities: smime_identities(),
        decrypt_probe: None,
        timings_ms: Timings::default(),
        errors: Vec::new(),
    };
    check_store(config, opts.sample, &mut r);
    if opts.anon {
        anonymise(&mut r);
    }
    r
}

/// Full Disk Access granted and the schema resolved: `doctor` exits 0.
pub fn healthy(r: &Report) -> bool {
    r.full_disk_access == "ok" && r.schema.is_some()
}

/// The value, or `None` with "`what`: error" noted.
fn note<T, E: Display>(errors: &mut Vec<String>, what: &str, res: Result<T, E>) -> Option<T> {
    res.map_err(|e| errors.push(format!("{what}: {e}"))).ok()
}

fn access(e: &StoreError) -> &'static str {
    match e {
        StoreError::MissingOrDenied { .. } => "missing or denied",
        StoreError::NoVersionDir { .. } | StoreError::Undetermined { .. } => "undetermined",
    }
}

fn check_store(config: &Config, sample: usize, r: &mut Report) {
    let store = match store::locate(&config.mail_root) {
        Ok(s) => s,
        Err(e) => {
            r.full_disk_access = access(&e);
            r.errors.push(format!("store: {e}"));
            return;
        }
    };
    r.version_dir = Some(store.version_dir.display().to_string());
    r.envelope_mtime =
        store::envelope_mtime(&store).map(|t| DateTime::<Local>::from(t).fixed_offset());
    match store::probe_access(&store) {
        Ok(_) => r.full_disk_access = "ok",
        Err(e) => {
            r.full_disk_access = access(&e);
            r.errors.push(format!("envelope index: {e}"));
        }
    }
    let start = Instant::now();
    let Some(ctx) = note(&mut r.errors, "open", read::open(config.clone())) else {
        return;
    };
    r.timings_ms.open = Some(start.elapsed().as_millis());
    check_index(&ctx, sample, r);
}

fn filter(scope: Scope, terms: Vec<String>, limit: usize) -> Filter {
    Filter {
        terms,
        scope,
        after: None,
        before: None,
        mailbox: None,
        account: None,
        from: None,
        unread: false,
        flagged: false,
        include_deleted: false,
        limit,
        offset: 0,
    }
}

fn check_index(ctx: &Context, sample: usize, r: &mut Report) {
    // The guard is released before `read::` calls lock the index again.
    let (rows, encrypted) = {
        let env = ctx.envelope();
        r.envelope_mode = Some(match env.mode {
            OpenMode::ReadOnly => "read-only",
            OpenMode::Snapshot(_) => "snapshot",
        });
        let s = env.schema();
        r.schema = Some(SchemaReport {
            optional_found: s.optional_found.clone(),
            optional_missing: s.optional_missing.clone(),
            has_recipients: s.has_recipients,
            has_summaries: s.has_summaries,
        });
        r.epoch = Some(if env.epoch_offset() == 0 {
            "unix"
        } else {
            "cocoa"
        });
        r.counts = note(&mut r.errors, "counts", env.count());
        r.accounts = note(&mut r.errors, "accounts", env.accounts()).unwrap_or_default();
        if let Some(boxes) = note(&mut r.errors, "mailboxes", env.mailboxes(None)) {
            let uuid = boxes
                .iter()
                .filter(|b| parse_mailbox_url(&b.url).account_uuid.is_some())
                .count();
            r.url_form = match uuid {
                _ if boxes.is_empty() => None,
                0 => Some("imap-user-host"),
                n if n == boxes.len() => Some("mailbox-uuid"),
                _ => Some("mixed"),
            };
        }
        let rows = note(
            &mut r.errors,
            "sample",
            env.search(&filter(Scope::All, Vec::new(), sample)),
        );
        let encrypted = note(&mut r.errors, "encrypted ids", env.newest_encrypted_ids(1))
            .and_then(|ids| ids.first().copied())
            .and_then(|id| note(&mut r.errors, "encrypted row", env.get(id)).flatten());
        (rows, encrypted)
    };

    let mut plain = None;
    if let Some(rows) = &rows {
        let (paths, flags, id) = sample_files(ctx, rows);
        r.path_sample = Some(paths);
        r.flag_check = Some(flags);
        plain = id;
    }

    let start = Instant::now();
    let header = filter(Scope::Subject, vec!["a".into()], 25);
    if note(&mut r.errors, "search", read::search(ctx, &header)).is_some() {
        r.timings_ms.search_header = Some(start.elapsed().as_millis());
    }

    if let Some(id) = plain {
        let start = Instant::now();
        // The email is dropped unread.
        let email = read::get_email(ctx, id, 20_000, false).map_err(|e| match e {
            // Without the mailbox name, which the anonymised report must not show.
            ReadError::FileMissing { id, .. } => format!("message {id}: no file"),
            e => e.to_string(),
        });
        if note(&mut r.errors, "get_email", email).is_some() {
            r.timings_ms.get_email = Some(start.elapsed().as_millis());
        }
    }

    r.decrypt_probe = Some(decrypt_probe(ctx, encrypted.as_ref(), &mut r.errors));
}

/// Also returns the newest row neither flagged encrypted in the index nor enveloped: the get_email
/// timing sample, so that only the probe touches the keychain.
fn sample_files(ctx: &Context, rows: &[MessageRow]) -> (PathSample, FlagCheck, Option<i64>) {
    let mut paths = PathSample {
        sampled: rows.len(),
        formula: 0,
        walk: 0,
        missing: 0,
    };
    let mut flags = FlagCheck {
        sampled: rows.len(),
        encrypted_flag_and_type_agree: 0,
        disagree: 0,
    };
    let mut plain = None;
    for row in rows {
        let Ok(Some(located)) = ctx.locator.locate(row.id, &row.mailbox_url) else {
            paths.missing += 1;
            continue;
        };
        match located.by {
            Resolution::Formula => paths.formula += 1,
            Resolution::Walk => paths.walk += 1,
        }
        let Ok(file) = emlx::read(&located.emlx) else {
            continue;
        };
        let Ok(parsed) = mime::parse(&file.raw) else {
            continue;
        };
        if file.flags.encrypted() == parsed.is_enveloped() {
            flags.encrypted_flag_and_type_agree += 1;
        } else {
            flags.disagree += 1;
        }
        if plain.is_none() && !Flags(row.flags as u64).encrypted() && !parsed.is_enveloped() {
            plain = Some(row.id);
        }
    }
    (paths, flags, plain)
}

fn enveloped_der(ctx: &Context, row: &MessageRow) -> anyhow::Result<Vec<u8>> {
    let located = ctx
        .locator
        .locate(row.id, &row.mailbox_url)?
        .ok_or_else(|| anyhow::anyhow!("no file"))?;
    let file = emlx::read(&located.emlx)?;
    mime::parse(&file.raw)?
        .enveloped_der()
        .ok_or_else(|| anyhow::anyhow!("flagged encrypted, but no enveloped-data DER"))
}

/// `security cms -D -n` (`-n`: no content) or the configured openssl decryptor on the newest
/// enveloped message; whatever comes back on stdout is dropped.
fn decrypt_probe(
    ctx: &Context,
    row: Option<&MessageRow>,
    errors: &mut Vec<String>,
) -> DecryptProbe {
    let mut probe = DecryptProbe {
        method: ctx.decryptor.name(),
        tried: false,
        ok: false,
        status: None,
        stderr: String::new(),
        ms: 0,
    };
    let Some(row) = row else {
        return probe;
    };
    let what = format!("decrypt probe: message {}", row.id);
    let Some(der) = note(errors, &what, enveloped_der(ctx, row)) else {
        return probe;
    };
    let start = Instant::now();
    let result = match ctx.config.smime.decryptor {
        DecryptorKind::Security => smime::run(SecurityCms::command(true), &der, PROBE_TIMEOUT),
        // No dialog can wait here: the decryptor's own timeout.
        DecryptorKind::Openssl => ctx.decryptor.decrypt(&der),
    };
    probe.ms = start.elapsed().as_millis();
    probe.tried = true;
    match result {
        Ok(_) => {
            probe.ok = true;
            probe.status = Some(0);
        }
        Err(DecryptError::Failed { status, stderr }) => {
            probe.status = Some(status);
            probe.stderr = stderr;
        }
        Err(e) => probe.stderr = e.to_string(),
    }
    probe
}

/// `None` if `security` cannot be run or fails.
fn smime_identities() -> Option<u32> {
    let mut cmd = Command::new("/usr/bin/security");
    cmd.args(["find-identity", "-v", "-p", "smime"]);
    let out = smime::run(cmd, &[], smime::TIMEOUT).ok()?;
    Some(count_identities(&String::from_utf8_lossy(&out)))
}

/// Lines like `  1) 0123…CDEF "Name"`: a number, `) `, a hex hash.
fn count_identities(text: &str) -> u32 {
    let is_hash = |h: &str| h.len() >= 40 && h.bytes().all(|b| b.is_ascii_hexdigit());
    text.lines()
        .filter(|l| {
            l.trim_start().split_once(") ").is_some_and(|(n, rest)| {
                !n.is_empty()
                    && n.bytes().all(|b| b.is_ascii_digit())
                    && rest.split(' ').next().is_some_and(is_hash)
            })
        })
        .count() as u32
}

fn anonymise(r: &mut Report) {
    let root = std::mem::replace(&mut r.mail_root, "<redacted>".into());
    r.version_dir = r.version_dir.as_deref().map(|v| {
        Path::new(v)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    r.accounts.clear();
    r.excluded_accounts = match r.excluded_accounts.len() {
        0 => Vec::new(),
        1 => vec!["<1 pattern>".into()],
        n => vec![format!("<{n} patterns>")],
    };
    for e in &mut r.errors {
        *e = scrub(e, &root);
    }
    // The status carries the signal; later lines may name certificates.
    if let Some(p) = &mut r.decrypt_probe {
        p.stderr = scrub(p.stderr.lines().next().unwrap_or_default(), &root);
    }
}

/// The `--anon` filter for free text. The literal `mail_root` becomes `<root>`. A path (from `<root>`,
/// or from a `/` or `~/` that starts a word) or a URL (`scheme://` starting a word) runs to the next
/// `": "` or line end and becomes `<root>` / `<root>/…`, `<path>` or `<url>`; spaces in folder
/// names stay inside it. Then every whitespace-separated token holding an `@` becomes `<addr>`.
pub fn scrub(text: &str, mail_root: &str) -> String {
    let text = match mail_root {
        "" => text.to_string(),
        root => text.replace(root, ROOT),
    };
    let mut out = String::new();
    let mut rest = text.as_str();
    let mut word_start = true;
    while let Some(c) = rest.chars().next() {
        let kind = if rest.starts_with(ROOT) {
            Some(ROOT)
        } else if word_start {
            span_kind(rest)
        } else {
            None
        };
        if let Some(kind) = kind {
            let end = [rest.find(": "), rest.find('\n')]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(rest.len());
            out.push_str(if kind == ROOT && end > ROOT.len() {
                "<root>/…"
            } else {
                kind
            });
            rest = &rest[end..];
            word_start = false;
            continue;
        }
        out.push(c);
        word_start = c.is_whitespace() || "\"'([=,:".contains(c);
        rest = &rest[c.len_utf8()..];
    }
    out.split_inclusive(char::is_whitespace)
        .map(|piece| {
            let token = piece.trim_end_matches(char::is_whitespace);
            if token.contains('@') {
                format!("<addr>{}", &piece[token.len()..])
            } else {
                piece.to_string()
            }
        })
        .collect()
}

/// `<path>` for `/…` or `~/…`, `<url>` for `scheme://…`.
fn span_kind(s: &str) -> Option<&'static str> {
    if s.starts_with('/') || s.starts_with("~/") {
        return Some("<path>");
    }
    let scheme = &s[..s.find("://")?];
    let is_scheme = !scheme.is_empty()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b));
    is_scheme.then_some("<url>")
}

pub fn render(r: &Report, json: bool) -> String {
    if json {
        return serde_json::to_string_pretty(r).expect("the report serialises") + "\n";
    }
    let mut out = String::new();
    let mut line = |mark: &str, item: &str, value: String| {
        out += &format!(
            "{mark:<4} {item}: {}\n",
            value.lines().collect::<Vec<_>>().join(" | ")
        );
    };
    let mark = |good: bool| if good { "ok" } else { "FAIL" };
    let yes = |b: bool| if b { "yes" } else { "no" };

    line("ok", "importin", r.version.clone());
    line("ok", "mail root", r.mail_root.clone());
    line(
        mark(r.full_disk_access == "ok"),
        "full disk access",
        r.full_disk_access.into(),
    );
    match &r.version_dir {
        Some(v) => line("ok", "store", v.clone()),
        None => line("--", "store", "not found".into()),
    }
    match (&r.schema, &r.version_dir) {
        (Some(s), _) => {
            let mut v = format!(
                "{} of {} optional columns, recipients {}, summaries {}",
                s.optional_found.len(),
                s.optional_found.len() + s.optional_missing.len(),
                yes(s.has_recipients),
                yes(s.has_summaries)
            );
            if !s.optional_missing.is_empty() {
                v += &format!("; missing {}", s.optional_missing.join(", "));
            }
            line("ok", "schema", v);
        }
        (None, Some(_)) => line("FAIL", "schema", "unresolved".into()),
        (None, None) => line("--", "schema", "not checked".into()),
    }
    match r.envelope_mode {
        Some(m) => line("ok", "envelope index", m.into()),
        None => line("--", "envelope index", "not opened".into()),
    }
    match r.epoch {
        Some(e) => line("ok", "epoch", e.into()),
        None => line("--", "epoch", "not detected".into()),
    }
    match &r.counts {
        Some(c) => line(
            "ok",
            "counts",
            format!(
                "{} messages, {} mailboxes, {} accounts, {} excluded messages",
                c.messages, c.mailboxes, c.accounts, c.excluded_messages
            ),
        ),
        None => line("--", "counts", "not counted".into()),
    }
    if r.excluded_accounts.is_empty() {
        line("ok", "excluded accounts", "none".into());
    } else {
        line("ok", "excluded accounts", r.excluded_accounts.join(", "));
    }
    if r.anon {
        line("--", "accounts", "redacted".into());
    } else if r.accounts.is_empty() {
        line("--", "accounts", "none listed".into());
    } else {
        let accounts: Vec<String> = r
            .accounts
            .iter()
            .map(|a| {
                format!(
                    "{} ({} mailboxes, {} messages)",
                    a.identifier, a.mailboxes, a.messages
                )
            })
            .collect();
        line("ok", "accounts", accounts.join(", "));
    }
    line("ok", "mail running", yes(r.mail_running).into());
    match r.envelope_mtime {
        Some(t) => line("ok", "envelope index mtime", t.to_rfc3339()),
        None => line("--", "envelope index mtime", "unknown".into()),
    }
    match r.url_form {
        Some(f) => line("ok", "mailbox url form", f.into()),
        None => line("--", "mailbox url form", "unknown".into()),
    }
    match &r.path_sample {
        Some(p) => line(
            mark(p.missing == 0),
            "path sample",
            format!(
                "{} sampled: {} formula, {} walk, {} missing",
                p.sampled, p.formula, p.walk, p.missing
            ),
        ),
        None => line("--", "path sample", "not run".into()),
    }
    match &r.flag_check {
        Some(f) => line(
            mark(f.disagree == 0),
            "flag check",
            format!(
                "{} sampled: {} agree, {} disagree, {} unreadable",
                f.sampled,
                f.encrypted_flag_and_type_agree,
                f.disagree,
                f.sampled - f.encrypted_flag_and_type_agree - f.disagree
            ),
        ),
        None => line("--", "flag check", "not run".into()),
    }
    match r.smime_identities {
        Some(n) => line("ok", "s/mime identities", n.to_string()),
        None => line("--", "s/mime identities", "undetermined".into()),
    }
    match &r.decrypt_probe {
        None => line("--", "decrypt probe", "not run".into()),
        Some(p) if !p.tried => line(
            "--",
            &format!("decrypt probe ({})", p.method),
            "not tried".into(),
        ),
        Some(p) if p.ok => line(
            "ok",
            &format!("decrypt probe ({})", p.method),
            format!("{} ms", p.ms),
        ),
        Some(p) => {
            let status = p
                .status
                .map_or("no exit status".into(), |s| format!("exit {s}"));
            line(
                "FAIL",
                &format!("decrypt probe ({})", p.method),
                format!("{status} after {} ms: {}", p.ms, p.stderr),
            );
        }
    }
    let t = &r.timings_ms;
    for (name, ms) in [
        ("time open", t.open),
        ("time search_header", t.search_header),
        ("time get_email", t.get_email),
    ] {
        match ms {
            Some(ms) => line("ok", name, format!("{ms} ms")),
            None => line("--", name, "not run".into()),
        }
    }
    for e in &r.errors {
        line("FAIL", "error", e.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_identity_lines_only() {
        // The `-v` form: the valid identities only.
        let out = "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Paul Example\"\n  \
            2) FEDCBA9876543210FEDCBA9876543210FEDCBA98 \"other\"\n     \
            2 valid identities found\n";
        assert_eq!(count_identities(out), 2);
        assert_eq!(count_identities("     0 valid identities found\n"), 0);
    }
}
