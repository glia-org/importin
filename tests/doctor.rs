//! `doctor` on the synthetic store: the report, its `--anon` form, the text rendering and the CLI.

mod support;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use importin::config::{self, Config, DecryptorKind};
use importin::doctor::{self, Options, Report};
use serde_json::Value;
use support::fixture::{self, Epoch, Fixture, Options as FixtureOptions, UrlForm};

fn config_at(mail_root: &Path) -> Config {
    let env = HashMap::from([(
        "IMPORTIN_MAIL_ROOT".to_string(),
        mail_root.display().to_string(),
    )]);
    config::parse("", &env).unwrap()
}

/// With the fixture's key and cert the probe decrypts with openssl, so no test consults the keychain.
fn config(fx: &Fixture) -> Config {
    let mut c = config_at(&fx.mail_root);
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
    let report = doctor::run(&config_at(&root), &opts);
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

    let p = r.decrypt_probe.as_ref().unwrap();
    match fx.ids.encrypted {
        Some(_) => {
            assert_eq!(p.method, "openssl");
            assert!(p.tried && p.ok, "probe failed: {:?} {}", p.status, p.stderr);
            assert_eq!(p.status, Some(0));
        }
        None => assert!(!p.tried),
    }
    let t = &r.timings_ms;
    assert!(t.open.is_some() && t.search_header.is_some() && t.get_email.is_some());
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
}

#[test]
fn anon_errors_lose_the_path_after_mail() {
    let (_dir, r) = missing_root_report(true);
    assert!(!r.errors.is_empty());
    for e in &r.errors {
        assert!(!e.contains("secret-box"), "{e}");
        assert!(e.contains("Mail/…"), "{e}");
    }
    let (_dir, plain) = missing_root_report(false);
    assert!(plain.errors.iter().any(|e| e.contains("Mail/secret-box")));
}

#[test]
fn anon_scrub_hides_addresses_and_paths() {
    let home = std::env::var("HOME").unwrap();
    let text = format!(
        "{home}/Library/Mail/V10/MailData/Envelope Index: denied\nrecipient <paul@example.test> not found"
    );
    let s = doctor::scrub(&text);
    assert_eq!(s, "~/Library/Mail/…: denied\nrecipient <addr> not found");
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
