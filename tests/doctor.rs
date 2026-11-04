//! `doctor` on the synthetic store: the report, its `--anon` form, the text rendering and the CLI.

mod support;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use importin::config::{self, Config, DecryptorKind};
use importin::doctor::{self, Options, Report};
use serde_json::Value;
use support::fixture::{self, Epoch, Fixture, Options as FixtureOptions, UrlForm};

/// `accounts_db` keeps the tests off the real `~/Library/Accounts`; it need not exist.
fn config_at(mail_root: &Path, accounts_db: &Path) -> Config {
    let env = HashMap::from([
        (
            "IMPORTIN_MAIL_ROOT".to_string(),
            mail_root.display().to_string(),
        ),
        (
            "IMPORTIN_ACCOUNTS_DB".to_string(),
            accounts_db.display().to_string(),
        ),
    ]);
    config::parse("", &env).unwrap()
}

/// With the fixture's key and cert the probe decrypts with openssl, so no test consults the keychain.
fn config(fx: &Fixture) -> Config {
    let mut c = config_at(&fx.mail_root, &fx.accounts_db);
    if let Some(keys) = &fx.smime {
        c.smime.decryptor = DecryptorKind::Openssl;
        c.smime.key = Some(keys.key.clone());
        c.smime.cert = Some(keys.cert.clone());
    }
    c
}

fn missing_root_report(anon: bool) -> (tempfile::TempDir, Report) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Mail").join("secret-box");
    let opts = Options {
        anon,
        ..Options::default()
    };
    let report = doctor::run(
        &config_at(&root, &dir.path().join("Accounts4.sqlite")),
        &opts,
    );
    (dir, report)
}

#[test]
fn report_on_fixture_is_ok() {
    let fx = fixture::build(Epoch::Unix);
    let r = doctor::run(&config(&fx), &Options::default());
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.full_disk_access, "ok");
    assert!(r.schema.is_some());
    assert!(doctor::healthy(&r));
    assert_eq!(
        r.version_dir.as_deref(),
        Some(&*fx.mail_root.join("V10").display().to_string())
    );
    assert_eq!(r.envelope_mode, Some("read-only"));
    assert_eq!(r.epoch, Some("unix"));
    assert!(r.counts.unwrap().messages >= 40);
    assert_eq!(r.url_form, Some("imap-user-host"));
    assert!(
        r.accounts
            .iter()
            .any(|a| a.identifier == "paul@example.test")
    );
    assert!(r.envelope_mtime.is_some());

    let s = r.path_sample.as_ref().unwrap();
    assert_eq!(s.sampled, 20);
    assert_eq!(s.formula, s.sampled);
    assert_eq!((s.walk, s.missing), (0, 0));
    let f = r.flag_check.as_ref().unwrap();
    assert_eq!(f.sampled, 20);
    assert_eq!((f.encrypted_flag_and_type_agree, f.disagree), (20, 0));
    assert_eq!((f.flag_set_not_enveloped, f.enveloped_flag_clear), (0, 0));

    let p = r.decrypt_probe.as_ref().unwrap();
    match fx.ids.encrypted {
        Some(_) => {
            assert_eq!(p.method, "openssl");
            assert!(
                p.tried && p.ok,
                "probe failed: {:?} {:?}",
                p.status,
                p.stderr
            );
            assert_eq!(p.status, Some(0));
        }
        None => assert!(!p.tried),
    }
    let t = &r.timings_ms;
    assert!(t.open.is_some() && t.search_header.is_some() && t.get_email.is_some());
    assert!(t.search_all.is_some() && t.search_participant.is_some());
    let text = doctor::render(&r, false);
    for name in ["search_header", "search_all", "search_participant"] {
        let prefix = format!("ok   time {name}: ");
        assert!(text.lines().any(|l| l.starts_with(&prefix)), "{text}");
    }
}

#[test]
fn url_form_follows_the_mailbox_urls() {
    let fx = fixture::build_with(
        Epoch::Cocoa,
        FixtureOptions {
            url_form: UrlForm::MailboxUuid,
            ..FixtureOptions::default()
        },
    );
    let r = doctor::run(&config(&fx), &Options::default());
    assert_eq!(r.url_form, Some("mailbox-uuid"));
    assert_eq!(r.epoch, Some("cocoa"));
}

#[test]
fn anon_report_has_no_addresses() {
    let fx = fixture::build(Epoch::Unix);
    let mut c = config(&fx);
    c.exclude_accounts = vec!["*@gmail.example".into(), "nobody@example.test".into()];
    let opts = Options {
        anon: true,
        ..Options::default()
    };
    let r = doctor::run(&c, &opts);
    assert_eq!(r.full_disk_access, "ok");
    assert_eq!(r.mail_root, "<redacted>");
    assert_eq!(r.version_dir.as_deref(), Some("V10"));
    assert!(r.accounts.is_empty());
    assert_eq!(r.excluded_accounts, vec!["<2 patterns>".to_string()]);
    let json = serde_json::to_string(&r).unwrap();
    for needle in ["@", "example.test", "paul"] {
        assert!(!json.contains(needle), "{needle:?} in {json}");
    }
    assert!(r.anon);
    let text = doctor::render(&r, false);
    assert!(text.contains("--   accounts: redacted"), "{text}");
}

#[test]
fn anon_errors_hide_the_mail_root() {
    let dir = tempfile::tempdir().unwrap();
    // Missing roots name themselves; a root with a V10 but no Envelope Index names a path below it.
    let under_mail = dir.path().join("Mail").join("secret-box");
    let custom = dir.path().join("custom root").join("store");
    let bare = dir.path().join("bare root");
    std::fs::create_dir_all(bare.join("V10")).unwrap();
    let dir_text = dir.path().display().to_string();
    let no_accounts = dir.path().join("Accounts4.sqlite");
    for root in [&under_mail, &custom, &bare] {
        let plain = doctor::run(&config_at(root, &no_accounts), &Options::default());
        let root_text = root.display().to_string();
        assert!(
            plain.errors.iter().any(|e| e.contains(&root_text)),
            "{:?}",
            plain.errors
        );

        let opts = Options {
            anon: true,
            ..Options::default()
        };
        let r = doctor::run(&config_at(root, &no_accounts), &opts);
        assert!(
            r.errors.iter().any(|e| e.contains("<root>")),
            "{:?}",
            r.errors
        );
        for e in &r.errors {
            for gone in [
                &dir_text[..],
                "secret-box",
                "custom root",
                "bare root",
                "V10",
                "MailData",
            ] {
                assert!(!e.contains(gone), "{gone:?} in {e}");
            }
        }
    }
}

#[test]
fn anon_scrub_hides_paths_urls_and_addresses() {
    let root = "/srv/mail store of paul";
    let cases = [
        (
            "/srv/mail store of paul/V10/ABCD/Private.mbox/x.emlx: denied",
            "<root>/…: denied",
        ),
        ("/srv/mail store of paul: missing", "<root>: missing"),
        (
            "open /Volumes/Private Disk/keys/my key.pem: no such file",
            "open <path>: no such file",
        ),
        (
            "~/Documents/secret plan.txt: unreadable",
            "<path>: unreadable",
        ),
        (
            "message 7: no file in mailbox://4F3B2A10-8C1D-4E5F-9A6B-7C8D9E0F1A2B/Bewerbung Müller",
            "message 7: no file in <url>",
        ),
        (
            "url \"imap://paul%40example.test@mail.example.test/INBOX/Private Stuff\": bad",
            "url \"<url>: bad",
        ),
        (
            "recipient <someone@example.test> unknown\nsee and/or 1/2",
            "recipient <addr> unknown\nsee and/or 1/2",
        ),
    ];
    let mut all = String::new();
    for (text, want) in cases {
        let got = doctor::scrub(text, root);
        assert_eq!(got, want, "{text}");
        all += &got;
    }
    for gone in [
        "mail store",
        "paul",
        "ABCD",
        "Private",
        "my key",
        "secret plan",
        "4F3B2A10",
        "Bewerbung",
        "Müller",
        "example.test",
        "someone",
        "@",
    ] {
        assert!(!all.contains(gone), "{gone:?} in {all}");
    }
}

#[test]
fn anon_probe_drops_stderr() {
    let fx = fixture::build(Epoch::Unix);
    let Some(keys) = &fx.smime else {
        eprintln!("skipped: no openssl, no encrypted message");
        return;
    };
    // A certificate is no private key: openssl fails, naming the file.
    let mut c = config(&fx);
    c.smime.key = Some(keys.cert.clone());
    let p = doctor::run(&c, &Options::default()).decrypt_probe.unwrap();
    assert!(p.tried && !p.ok);
    assert!(p.stderr.as_deref().is_some_and(|e| !e.is_empty()), "{p:?}");

    let opts = Options {
        anon: true,
        ..Options::default()
    };
    let r = doctor::run(&c, &opts);
    let p = r.decrypt_probe.as_ref().unwrap();
    assert!(p.tried && !p.ok && p.status.is_some(), "{p:?}");
    assert_eq!((p.method, p.stderr.as_deref()), ("openssl", None));
    let v: Value = serde_json::from_str(&doctor::render(&r, true)).unwrap();
    let probe = v["decrypt_probe"].as_object().unwrap();
    let mut keys: Vec<&str> = probe.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["method", "ms", "ok", "status", "tried"]);
    let text = doctor::render(&r, false);
    let line = text.lines().find(|l| l.contains("decrypt probe")).unwrap();
    assert!(line.starts_with("FAIL") && line.ends_with(" ms"), "{line}");
}

/// Rewrites the emlx plist `flags` of a message of the synthetic store (never Mail's).
fn rewrite_emlx_flags(fx: &Fixture, id: i64, f: impl Fn(u64) -> u64) {
    let store = importin::store::locate(&fx.mail_root).unwrap();
    let env = importin::envelope::open(&store, &[], &Default::default()).unwrap();
    let url = env.get(id).unwrap().unwrap().mailbox_url;
    let located = importin::paths::Locator::new(store)
        .locate(id, &url)
        .unwrap();
    let path = located.unwrap().emlx;
    let text = std::fs::read_to_string(&path).unwrap();
    let key = text.rfind("<key>flags</key>").unwrap();
    let start = key + text[key..].find("<integer>").unwrap() + "<integer>".len();
    let end = start + text[start..].find("</integer>").unwrap();
    let flags = f(text[start..end].parse().unwrap());
    std::fs::write(&path, format!("{}{flags}{}", &text[..start], &text[end..])).unwrap();
}

#[test]
fn flag_check_counts_each_kind_of_disagreement() {
    let fx = fixture::build(Epoch::Unix);
    rewrite_emlx_flags(&fx, fx.ids.plain, |f| f | 8);
    let clear = match fx.ids.encrypted {
        Some(id) => {
            rewrite_emlx_flags(&fx, id, |f| f & !8);
            1
        }
        None => 0,
    };
    let r = doctor::run(&config(&fx), &Options::default());
    let f = r.flag_check.as_ref().unwrap();
    assert_eq!(
        (f.flag_set_not_enveloped, f.enveloped_flag_clear),
        (1, clear)
    );
    assert_eq!(f.disagree, 1 + clear);
    assert_eq!(f.encrypted_flag_and_type_agree, 20 - 1 - clear);
    let text = doctor::render(&r, false);
    let expected = format!(
        "FAIL flag check: 20 sampled: {} agree, {} disagree, 0 unreadable",
        19 - clear,
        1 + clear
    );
    assert!(text.lines().any(|l| l == expected), "{text}");
}

#[test]
fn get_email_timing_skips_encrypted_messages() {
    let fx = fixture::build(Epoch::Unix);
    let index = fx.mail_root.join("V10/MailData/Envelope Index");
    rusqlite::Connection::open(index)
        .unwrap()
        .execute("UPDATE messages SET flags = flags | 8", [])
        .unwrap();
    let r = doctor::run(&config(&fx), &Options::default());
    assert!(r.timings_ms.get_email.is_none());
    assert!(
        r.errors.iter().all(|e| !e.starts_with("get_email")),
        "{:?}",
        r.errors
    );
    let text = doctor::render(&r, false);
    assert!(text.contains("--   time get_email"), "{text}");
}

#[test]
fn missing_root_reports_denied() {
    let (_dir, r) = missing_root_report(false);
    assert_eq!(r.full_disk_access, "missing or denied");
    assert!(r.version_dir.is_none() && r.schema.is_none() && r.counts.is_none());
    assert!(r.path_sample.is_none() && r.flag_check.is_none() && r.decrypt_probe.is_none());
    assert!(r.timings_ms.open.is_none() && r.timings_ms.get_email.is_none());
    assert!(!doctor::healthy(&r));
}

#[test]
fn text_render_marks_failures() {
    let (_dir, r) = missing_root_report(false);
    let text = doctor::render(&r, false);
    assert!(
        text.lines()
            .any(|l| l.starts_with("FAIL") && l.contains("full disk access")),
        "{text}"
    );
    assert!(text.lines().any(|l| l.starts_with("--")), "{text}");

    let fx = fixture::build(Epoch::Unix);
    let ok = doctor::render(&doctor::run(&config(&fx), &Options::default()), false);
    assert!(!ok.contains("FAIL"), "{ok}");
    assert!(
        ok.lines()
            .all(|l| l.starts_with("ok ") || l.starts_with("-- ")),
        "{ok}"
    );
    assert!(ok.contains(" ms"), "{ok}");
}

#[test]
fn json_render_is_the_report() {
    let (_dir, r) = missing_root_report(false);
    let v: Value = serde_json::from_str(&doctor::render(&r, true)).unwrap();
    assert_eq!(v["full_disk_access"], "missing or denied");
    assert!(v["schema"].is_null());
}

fn cli(fx: &Fixture, mail_root: &Path) -> std::process::Output {
    let mut toml = String::new();
    if let Some(keys) = &fx.smime {
        toml += &format!(
            "[smime]\ndecryptor = \"openssl\"\nkey = {:?}\ncert = {:?}\n",
            keys.key.display().to_string(),
            keys.cert.display().to_string()
        );
    }
    let path = fx.dir.path().join("config.toml");
    std::fs::write(&path, toml).unwrap();
    Command::new(env!("CARGO_BIN_EXE_importin"))
        .args(["doctor", "--anon", "--json"])
        .env("IMPORTIN_MAIL_ROOT", mail_root)
        .env("IMPORTIN_ACCOUNTS_DB", &fx.accounts_db)
        .env("IMPORTIN_CONFIG", &path)
        .output()
        .unwrap()
}

#[test]
fn cli_prints_json_and_sets_the_exit_code() {
    let fx = fixture::build(Epoch::Unix);
    let out = cli(&fx, &fx.mail_root);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["full_disk_access"], "ok");
    assert_eq!(v["mail_root"], "<redacted>");

    let out = cli(&fx, &fx.dir.path().join("nowhere"));
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["full_disk_access"], "missing or denied");
}

#[test]
fn accounts_carry_label_kind_and_uuid_unless_anon() {
    let fx = fixture::build_with(
        Epoch::Unix,
        FixtureOptions {
            url_form: UrlForm::MailboxUuid,
            ..FixtureOptions::default()
        },
    );
    let r = doctor::run(&config(&fx), &Options::default());
    let a = &r.accounts[0];
    assert_eq!(a.identifier, "paul@example.test");
    assert_eq!(a.label.as_deref(), Some("Example"));
    assert_eq!(a.kind.as_deref(), Some("IMAP"));
    assert_eq!(a.uuid.as_deref(), Some(fixture::ACCOUNT_A));
    let v: Value = serde_json::from_str(&doctor::render(&r, true)).unwrap();
    assert_eq!(v["accounts"][1]["label"], "Google");
    let text = doctor::render(&r, false);
    let expected = format!(
        "paul@example.test (Example; IMAP; {}; 4 mailboxes, ",
        fixture::ACCOUNT_A
    );
    assert!(text.contains(&expected), "{text}");

    let opts = Options {
        anon: true,
        ..Options::default()
    };
    let r = doctor::run(&config(&fx), &opts);
    assert!(r.accounts.is_empty());
    let json = serde_json::to_string(&r).unwrap();
    for needle in [
        fixture::ACCOUNT_A,
        fixture::ACCOUNT_B,
        "Example",
        "Google",
        "IMAP",
    ] {
        assert!(!json.contains(needle), "{needle:?} in {json}");
    }
}

#[test]
fn cli_prints_an_unknown_schema_to_stderr_only() {
    let fx = fixture::build(Epoch::Unix);
    // The fixture's own synthetic index, never Mail's.
    rusqlite::Connection::open(fx.mail_root.join("V10/MailData/Envelope Index"))
        .unwrap()
        .execute_batch("ALTER TABLE mailboxes RENAME COLUMN url TO link;")
        .unwrap();
    let out = cli(&fx, &fx.mail_root);
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["errors"][0],
        "open: unknown Envelope Index schema: missing mailboxes.url"
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("CREATE TABLE"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("CREATE TABLE mailboxes (ROWID INTEGER PRIMARY KEY, link TEXT"),
        "{stderr}"
    );
}
