//! The MCP server over stdio, driven by a tiny newline-delimited JSON-RPC client.

mod support;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

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

/// A running `importin serve`; killed on drop.
struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
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
            .env("IMPORTIN_CONFIG", &config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
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
            next_id: 1,
            _config: dir,
        }
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
    assert!(
        props["scope"]["description"].as_str().is_some(),
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
    assert!(r["messages"][0]["to"].is_array());

    let r = s.ok("get_thread", json!({"id": fx.ids.thread[0], "limit": 2}));
    assert_eq!(hit_ids(&r, "messages"), fx.ids.thread[..2]);

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
