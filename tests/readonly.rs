//! Read-only by construction (DESIGN §2): no file under `src/` names a way to send, move, flag or
//! delete mail, to script Mail.app, to write a file, or to open SQLite for writing. The forbidden
//! words may appear in `tests/`, which build the fixture.

use std::path::{Path, PathBuf};

/// Matched case-insensitively against every line of `src/**/*.rs`. `imap://` is not here: the
/// envelope module parses mailbox URLs of that scheme.
const FORBIDDEN: [&str; 14] = [
    "osascript",
    "ImapClient",
    "smtp",
    "send_mail",
    "move_message",
    "delete_message",
    "set_flag",
    "SQLITE_OPEN_READ_WRITE",
    "SQLITE_OPEN_CREATE",
    "immutable=1",
    "fs::write",
    "File::create",
    "OpenOptions",
    "Connection::open(",
];

/// Lines before an `open_with_flags` call that may hold its flags.
const FLAGS_WINDOW: usize = 6;

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<(PathBuf, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    assert!(!files.is_empty(), "no sources under {}", src.display());
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            (p, text)
        })
        .collect()
}

#[test]
fn src_names_no_write_path() {
    let mut hits = Vec::new();
    for (path, text) in sources() {
        for (n, line) in text.lines().enumerate() {
            let lower = line.to_lowercase();
            for word in FORBIDDEN {
                if lower.contains(&word.to_lowercase()) {
                    hits.push(format!("{}:{}: {word}", path.display(), n + 1));
                }
            }
        }
    }
    assert!(hits.is_empty(), "forbidden in src/:\n{}", hits.join("\n"));
}

#[test]
fn sqlite_is_opened_read_only() {
    let mut calls = 0;
    let mut bad = Vec::new();
    for (path, text) in sources() {
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            if !line.contains("open_with_flags") {
                continue;
            }
            calls += 1;
            let window = &lines[n.saturating_sub(FLAGS_WINDOW)..=n];
            if !window.iter().any(|l| l.contains("SQLITE_OPEN_READ_ONLY")) {
                bad.push(format!("{}:{}", path.display(), n + 1));
            }
        }
    }
    // Not vacuous: the Envelope Index is opened somewhere.
    assert!(calls > 0, "no open_with_flags call found in src/");
    assert!(
        bad.is_empty(),
        "open_with_flags without SQLITE_OPEN_READ_ONLY in the {FLAGS_WINDOW} lines before:\n{}",
        bad.join("\n")
    );
}
