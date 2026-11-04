//! The MCP server over stdio, driven by a tiny newline-delimited JSON-RPC client.

mod support;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use support::fixture::{self, Epoch, Fixture};

const TOOLS: [&str; 6] = [
    "attachment_text",
    "get_email",
    "get_thread",
    "list_accounts",
    "list_mailboxes",
    "search",
];

/// The children's timezone: +05:30 all year (no DST), so a `YYYY-MM-DD` bound is local midnight at
/// a fixed, non-whole-hour distance from UTC midnight.
const TZ: &str = "Asia/Kolkata";
const TZ_OFFSET_MINUTES: i64 = 330;

/// A running `importin serve`; killed on drop.
struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    /// Echoes the child's stderr and returns all of it at EOF.
    stderr: Option<JoinHandle<String>>,
    next_id: i64,
    _config: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// Spawns the binary against the fixture; account B excluded, openssl S/MIME when available.
    fn spawn(fx: &Fixture) -> Server {
        Server::spawn_with(fx, &[])
    }

    fn spawn_with(fx: &Fixture, env: &[(&str, &str)]) -> Server {
        let dir = tempfile::tempdir().unwrap();
        let mut toml = String::from("exclude_accounts = [\"*@gmail.example\"]\n");
        if let Some(keys) = &fx.smime {
            toml += &format!(
                "[smime]\ndecryptor = \"openssl\"\nkey = {:?}\ncert = {:?}\n",
                keys.key.display().to_string(),
                keys.cert.display().to_string()
            );
        }
        let config = dir.path().join("config.toml");
        std::fs::write(&config, toml).unwrap();

        let mut child = Command::new(env!("CARGO_BIN_EXE_importin"))
            .env("IMPORTIN_MAIL_ROOT", &fx.mail_root)
            .env("IMPORTIN_ACCOUNTS_DB", &fx.accounts_db)
            .env("IMPORTIN_CONFIG", &config)
            .env("TZ", TZ)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut all = String::new();
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                eprintln!("{line}");
                all += &line;
                all.push('\n');
            }
            all
        });
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Server {
            child,
            stdin,
            lines,
            stderr: Some(stderr),
            next_id: 1,
            _config: dir,
        }
    }

    /// Kills the child and returns everything it wrote to stderr.
    fn finish(mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stderr.take().unwrap().join().unwrap()
    }

    /// Spawned, initialized, and told so.
    fn start(fx: &Fixture) -> Server {
        let mut s = Server::spawn(fx);
        s.initialize();
        s
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// The response to request `id`; every stdout line must be JSON.
    fn response(&mut self, id: i64) -> Value {
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|e| panic!("no response to request {id}: {e}"));
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line:?}"));
            if msg["id"] == json!(id) {
                return msg;
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.response(id)
    }

    fn initialize(&mut self) -> Value {
        let r = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "protocol-test", "version": "0"}
            }),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        r
    }

    /// The `result` of `tools/call`.
    fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        assert!(r.get("error").is_none(), "{tool}: protocol error {r}");
        r["result"].clone()
    }

    /// `structuredContent` of a successful call; the text content holds the same JSON.
    fn ok(&mut self, tool: &str, args: Value) -> Value {
        let r = self.call(tool, args);
        assert_eq!(r["isError"], json!(false), "{tool}: {r}");
        let text = r["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            parsed, r["structuredContent"],
            "{tool}: text and structuredContent differ"
        );
        r["structuredContent"].clone()
    }

    /// The message of a tool error.
    fn err(&mut self, tool: &str, args: Value) -> String {
        let r = self.call(tool, args);
        assert_eq!(r["isError"], json!(true), "{tool}: {r}");
        r["content"][0]["text"].as_str().unwrap().to_string()
    }
}

fn hit_ids(v: &Value, key: &str) -> Vec<i64> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_i64().unwrap())
        .collect()
}

fn addresses(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|a| a["address"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn tools_list_has_six_read_only_tools() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::spawn(&fx);
    let init = s.initialize();
    let info = &init["result"];
    assert_eq!(info["serverInfo"]["name"], "importin");
    assert_eq!(info["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        info["instructions"]
            .as_str()
            .unwrap()
            .starts_with("Read-only access to Apple Mail on this Mac.")
    );

    let r = s.request("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, TOOLS);
    for t in tools {
        let a = &t["annotations"];
        assert_eq!(a["readOnlyHint"], json!(true), "{t}");
        assert_eq!(a["destructiveHint"], json!(false), "{t}");
        assert_eq!(a["idempotentHint"], json!(true), "{t}");
        assert_eq!(a["openWorldHint"], json!(false), "{t}");
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
    }
    let search = tools.iter().find(|t| t["name"] == "search").unwrap();
    let props = &search["inputSchema"]["properties"];
    for p in [
        "query", "scope", "after", "before", "mailbox", "account", "from", "unread", "flagged",
        "limit", "offset",
    ] {
        assert!(props.get(p).is_some(), "search lacks {p}: {props}");
    }
    let scope = props["scope"]["description"].as_str();
    assert!(
        scope
            .is_some_and(|d| d.contains("the configured window (body_scan_days, default 90 days)")),
        "field docs become descriptions: {props}"
    );
}

#[test]
fn search_cc_scope_returns_structured_hits() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);
    let r = s.ok("search", json!({"query": "carol", "scope": "cc"}));
    assert_eq!(hit_ids(&r, "hits"), [fx.ids.cc_target]);
    for h in r["hits"].as_array().unwrap() {
        assert!(h["to"].is_array(), "{h}");
        assert!(h["cc"].is_array(), "{h}");
    }
    assert_eq!(addresses(&r["hits"][0]["cc"]), ["carol@example.test"]);
    assert_eq!(r["truncated"], json!(false));
}

#[test]
fn search_filters_from_dates_and_body() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);

    let r = s.ok("search", json!({"from": "alice@example.test"}));
    assert!(hit_ids(&r, "hits").contains(&fx.ids.plain), "{r}");
    for h in r["hits"].as_array().unwrap() {
        assert_eq!(h["from"]["address"], "alice@example.test", "{h}");
    }

    let r = s.ok(
        "search",
        json!({"query": "kickoff", "scope": "subject", "after": "2000-01-01"}),
    );
    assert_eq!(hit_ids(&r, "hits").len(), 3, "{r}");
    let r = s.ok(
        "search",
        json!({"query": "kickoff", "before": "2000-01-01T00:00:00+01:00"}),
    );
    assert!(hit_ids(&r, "hits").is_empty(), "{r}");

    let r = s.ok(
        "search",
        json!({"query": "quarterly numbers", "scope": "body"}),
    );
    assert_eq!(hit_ids(&r, "hits"), [fx.ids.plain]);
    assert!(r["scanned"].as_u64().unwrap() > 0, "{r}");

    let r = s.ok(
        "search",
        json!({"query": "", "scope": "subject", "limit": 2}),
    );
    assert_eq!(hit_ids(&r, "hits").len(), 2);
    assert_eq!(r["truncated"], json!(true));
}

#[test]
fn search_mailbox_unread_flagged_and_offset() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);

    let r = s.ok("search", json!({"mailbox": "archive"}));
    assert_eq!(hit_ids(&r, "hits"), [123, 124, 125, 126]);

    // Account B (129–131 unread, 117) is excluded; 110 exists only with openssl.
    let r = s.ok("search", json!({"unread": true, "limit": 200}));
    let mut unread = hit_ids(&r, "hits");
    unread.sort();
    let mut expected = vec![
        102, 105, 106, 107, 108, 109, 114, 115, 116, 120, 121, 122, 132, 133, 2001, 2002, 2004,
        12_345,
    ];
    expected.extend(fx.ids.encrypted);
    expected.sort();
    assert_eq!(unread, expected);

    let r = s.ok("search", json!({"flagged": true}));
    assert_eq!(hit_ids(&r, "hits"), [132, 134]);

    let r = s.ok(
        "search",
        json!({"query": "kickoff", "scope": "subject", "offset": 1}),
    );
    let [first, second, _] = fx.ids.thread;
    assert_eq!(hit_ids(&r, "hits"), [second, first]);
}

fn received(hit: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(hit["date_received"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn plain_dates_are_local_midnight() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);
    // The fixture dates every message some hours before today 00:00 UTC.
    let day = Utc::now().date_naive();
    let midnight =
        day.and_hms_opt(0, 0, 0).unwrap().and_utc() - chrono::Duration::minutes(TZ_OFFSET_MINUTES);
    let all = s.ok("search", json!({"limit": 200}));
    let hits = all["hits"].as_array().unwrap();
    let (newer, older): (Vec<&Value>, Vec<&Value>) =
        hits.iter().partition(|h| received(h) >= midnight);
    let ids = |v: &[&Value]| -> Vec<i64> { v.iter().map(|h| h["id"].as_i64().unwrap()).collect() };
    assert!(
        !newer.is_empty() && !older.is_empty(),
        "rows on both sides of {midnight}"
    );

    let r = s.ok("search", json!({"after": day.to_string(), "limit": 200}));
    assert_eq!(hit_ids(&r, "hits"), ids(&newer), "after {day} = {midnight}");
    let r = s.ok("search", json!({"before": day.to_string(), "limit": 200}));
    assert_eq!(
        hit_ids(&r, "hits"),
        ids(&older),
        "before {day} = {midnight}"
    );
}

#[test]
fn get_email_returns_recipients_and_body() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);
    let e = s.ok("get_email", json!({"id": fx.ids.cc_target}));
    assert_eq!(e["id"], json!(fx.ids.cc_target));
    assert_eq!(
        addresses(&e["to"]),
        ["paul@example.test", "bob@example.test"]
    );
    assert_eq!(addresses(&e["cc"]), ["carol@example.test"]);
    assert!(e["body"].as_str().unwrap().contains("budget review"), "{e}");
    assert_eq!(e["body_source"], "plain");

    let e = s.ok(
        "get_email",
        json!({"id": fx.ids.cc_target, "max_chars": 500}),
    );
    assert_eq!(e["truncated"], json!(false));
}

#[test]
fn get_email_decrypts_with_openssl_config() {
    let fx = fixture::build(Epoch::Unix);
    let Some(id) = fx.ids.encrypted else {
        eprintln!("skipped: the fixture has no S/MIME message (no openssl)");
        return;
    };
    let mut s = Server::start(&fx);
    let e = s.ok("get_email", json!({"id": id}));
    assert_eq!(e["encryption"]["decrypted"], json!(true), "{e}");
    assert!(e["body"].as_str().unwrap().contains("the sealed sentence"));
}

#[test]
fn get_thread_three_messages() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);
    let r = s.ok("get_thread", json!({"id": fx.ids.thread[1]}));
    assert_eq!(hit_ids(&r, "messages"), fx.ids.thread);
    assert_eq!(r["truncated"], json!(false));
    assert!(r["messages"][0]["to"].is_array());

    // The newest `limit`, oldest first.
    for anchor in fx.ids.thread {
        let r = s.ok("get_thread", json!({"id": anchor, "limit": 2}));
        assert_eq!(
            hit_ids(&r, "messages"),
            fx.ids.thread[1..],
            "anchor {anchor}"
        );
        assert_eq!(r["truncated"], json!(true));
    }
    let r = s.ok("get_thread", json!({"id": fx.ids.thread[0], "limit": 3}));
    assert_eq!(hit_ids(&r, "messages"), fx.ids.thread);
    assert_eq!(r["truncated"], json!(false));

    let msg = s.err("get_thread", json!({"id": fx.ids.excluded}));
    assert!(msg.contains("no message"), "{msg}");
}

#[test]
fn list_mailboxes_and_accounts_exclude_configured() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);

    let r = s.ok("list_accounts", json!({}));
    let accounts: Vec<&str> = r["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["identifier"].as_str().unwrap())
        .collect();
    assert_eq!(accounts, ["paul@example.test"]);

    let r = s.ok("list_mailboxes", json!({}));
    let boxes = r["mailboxes"].as_array().unwrap();
    assert_eq!(boxes.len(), 4, "{r}");
    assert!(
        boxes.iter().all(|b| b["account"] == "paul@example.test"),
        "{r}"
    );
    let inbox = boxes.iter().find(|b| b["name"] == "INBOX").unwrap();
    assert_eq!(inbox["messages"], json!(28));

    let r = s.ok("list_mailboxes", json!({"account": "gmail"}));
    assert!(r["mailboxes"].as_array().unwrap().is_empty(), "{r}");

    let r = s.ok("search", json!({"account": "gmail"}));
    assert!(r["hits"].as_array().unwrap().is_empty(), "{r}");
    let msg = s.err("get_email", json!({"id": fx.ids.excluded}));
    assert!(msg.contains("no message"), "{msg}");
}

#[test]
fn attachment_text_pdf() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);
    let r = s.ok(
        "attachment_text",
        json!({"id": fx.ids.with_pdf, "filename": "a.pdf"}),
    );
    assert_eq!(r["filename"], "a.pdf");
    assert_eq!(r["state"], "inline");
    assert!(r["text"].as_str().unwrap().contains("pdf sentence"), "{r}");

    let r = s.ok(
        "attachment_text",
        json!({"id": fx.ids.with_pdf, "index": 0}),
    );
    assert_eq!(r["filename"], "a.pdf");

    let msg = s.err(
        "attachment_text",
        json!({"id": fx.ids.not_downloaded, "filename": "slides.pdf"}),
    );
    assert!(msg.contains("never fetched"), "{msg}");
}

#[test]
fn bad_scope_is_tool_error() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);
    let msg = s.err("search", json!({"query": "x", "scope": "nope"}));
    assert!(msg.contains("scope"), "{msg}");
    assert!(msg.contains("participant"), "lists the valid scopes: {msg}");
}

#[test]
fn argument_errors_are_tool_errors() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::start(&fx);

    let msg = s.err("search", json!({"scope": "body"}));
    assert_eq!(
        msg,
        "scope body needs a query; use another scope for header-only filters"
    );
    let msg = s.err("search", json!({"query": "  ", "scope": "body"}));
    assert!(msg.starts_with("scope body needs a query"), "{msg}");

    let msg = s.err("search", json!({"after": "yesterday"}));
    assert!(msg.contains("after") && msg.contains("YYYY-MM-DD"), "{msg}");
    let msg = s.err("search", json!({"before": "2025-13-01"}));
    assert!(msg.contains("before"), "{msg}");
    for (after, before) in [("2025-02-01", "2025-01-01"), ("2025-01-01", "2025-01-01")] {
        let msg = s.err("search", json!({"after": after, "before": before}));
        assert_eq!(msg, "after must be before before");
    }

    for limit in [0, 201] {
        let msg = s.err("search", json!({"query": "x", "limit": limit}));
        assert!(msg.contains("limit") && msg.contains("200"), "{msg}");
    }

    for max in [499, 200_001] {
        let msg = s.err("get_email", json!({"id": fx.ids.plain, "max_chars": max}));
        assert!(msg.contains("max_chars") && msg.contains("200000"), "{msg}");
    }
    for max in [499, 100_001] {
        let msg = s.err(
            "attachment_text",
            json!({"id": fx.ids.with_pdf, "index": 0, "max_chars": max}),
        );
        assert!(msg.contains("max_chars") && msg.contains("100000"), "{msg}");
    }

    let msg = s.err("get_email", json!({"id": 999_999}));
    assert_eq!(msg, "no message with id 999999");
}

#[test]
fn first_reply_under_budget() {
    let fx = fixture::build(Epoch::Unix);
    // macOS delays the first exec of a freshly built binary from a new process by ~0.7 s (a policy
    // check); later execs take ~5 ms. Pay that once so the budget measures the server's start.
    Command::new(env!("CARGO_BIN_EXE_importin"))
        .arg("--version")
        .output()
        .unwrap();
    let start = Instant::now();
    let mut s = Server::spawn(&fx);
    let r = s.initialize();
    let elapsed = start.elapsed();
    assert!(r["result"]["serverInfo"].is_object(), "{r}");
    assert!(
        elapsed < Duration::from_millis(500),
        "first reply after {elapsed:?}"
    );
}

#[test]
fn stray_stdout_print_goes_to_stderr() {
    let fx = fixture::build(Epoch::Unix);
    let mut s = Server::spawn_with(&fx, &[("IMPORTIN_TEST_STRAY_PRINT", "1")]);
    s.initialize();
    let r = s.ok("list_accounts", json!({}));
    assert_eq!(r["accounts"][0]["identifier"], "paul@example.test");
    let stderr = s.finish();
    assert!(stderr.lines().any(|l| l == "stray"), "stderr: {stderr:?}");
}

#[test]
fn unknown_schema_prints_the_tables_to_stderr() {
    let fx = fixture::build(Epoch::Unix);
    // The fixture's own synthetic index, never Mail's.
    rusqlite::Connection::open(fx.mail_root.join("V10/MailData/Envelope Index"))
        .unwrap()
        .execute_batch("ALTER TABLE mailboxes RENAME COLUMN url TO link;")
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_importin"))
        .arg("serve")
        .env("IMPORTIN_MAIL_ROOT", &fx.mail_root)
        .env("IMPORTIN_ACCOUNTS_DB", &fx.accounts_db)
        .env("IMPORTIN_CONFIG", fx.dir.path().join("no-config.toml"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut lines = stderr.lines();
    assert_eq!(
        lines.next(),
        Some("importin: unknown Envelope Index schema: missing mailboxes.url"),
        "{stderr}"
    );
    assert!(
        lines
            .next()
            .is_some_and(|l| l.starts_with("CREATE TABLE addresses")),
        "{stderr}"
    );
    assert!(
        stderr.contains("CREATE TABLE mailboxes (ROWID INTEGER PRIMARY KEY, link TEXT"),
        "{stderr}"
    );
}

/// The DESIGN §2 budgets on a 2 000-message store. Run by `checks/budgets.sh` with `--release`;
/// a debug build measures the wrong binary.
#[test]
#[ignore = "release budgets: run checks/budgets.sh"]
fn budgets_release() {
    // Checked at run time: a const assert would stop the debug build of this file from compiling.
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(!cfg!(debug_assertions), "run checks/budgets.sh");
    }
    let fx = fixture::build_with(
        Epoch::Unix,
        fixture::Options {
            messages: 2000 - 42,
            ..Default::default()
        },
    );
    // The first exec of a fresh binary costs ~0.7 s before main on macOS; pay it once here.
    Command::new(env!("CARGO_BIN_EXE_importin"))
        .arg("--version")
        .output()
        .unwrap();

    let start = Instant::now();
    let mut s = Server::spawn(&fx);
    let r = s.initialize();
    let init = start.elapsed();
    assert!(r["result"]["serverInfo"].is_object(), "{r}");

    let start = Instant::now();
    let hits = s.ok("search", json!({"query": "Filler", "scope": "subject"}));
    let search = start.elapsed();
    let ids = hit_ids(&hits, "hits");
    assert_eq!(ids.len(), 25, "{hits}");

    let start = Instant::now();
    let e = s.ok("get_email", json!({"id": ids[0]}));
    let get_email = start.elapsed();
    assert!(
        e["body"]
            .as_str()
            .unwrap()
            .starts_with("Filler body number"),
        "{e}"
    );

    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let report = format!(
        "budgets: initialize {:.1} ms (< 50), search {:.1} ms (< 50), get_email {:.1} ms (< 30)",
        ms(init),
        ms(search),
        ms(get_email)
    );
    println!("{report}");
    assert!(
        init < Duration::from_millis(50)
            && search < Duration::from_millis(50)
            && get_email < Duration::from_millis(30),
        "{report}"
    );
}
