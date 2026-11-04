mod support;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{Duration, Utc};
use importin::config::{self, Config, DecryptorKind};
use importin::envelope::{self, Filter, Scope};
use importin::paths::Locator;
use importin::read::{self, Context, Email, ReadError, SearchResult};
use importin::smime::EncryptionInfo;
use importin::store;
use support::fixture::{self, Epoch, Fixture, Options, UrlForm};

const NARROW: &str = "narrow with after:, from: or mailbox:";
const MORE: &str = "more hits may exist; raise limit or use offset";

fn config(fx: &Fixture) -> Config {
    let env = HashMap::from([
        (
            "IMPORTIN_MAIL_ROOT".to_string(),
            fx.mail_root.display().to_string(),
        ),
        (
            "IMPORTIN_ACCOUNTS_DB".to_string(),
            fx.accounts_db.display().to_string(),
        ),
    ]);
    config::parse("", &env).unwrap()
}

fn with_openssl(mut c: Config, key: &Path, cert: &Path) -> Config {
    c.smime.decryptor = DecryptorKind::Openssl;
    c.smime.key = Some(key.to_path_buf());
    c.smime.cert = Some(cert.to_path_buf());
    c
}

fn ctx(fx: &Fixture) -> Context {
    read::open(config(fx)).unwrap()
}

fn filter(scope: Scope, terms: &[&str]) -> Filter {
    Filter {
        terms: terms.iter().map(|t| t.to_string()).collect(),
        scope,
        after: None,
        before: None,
        mailbox: None,
        account: None,
        from: None,
        unread: false,
        flagged: false,
        include_deleted: false,
        limit: 20,
        offset: 0,
    }
}

fn email(c: &Context, id: i64) -> Email {
    match read::get_email(c, id, usize::MAX, false) {
        Ok(e) => e,
        Err(e) => panic!("get_email({id}): {e}"),
    }
}

fn ids(r: &SearchResult) -> Vec<i64> {
    r.hits.iter().map(|h| h.row.id).collect()
}

fn emlx_path(fx: &Fixture, id: i64) -> std::path::PathBuf {
    let store = store::locate(&fx.mail_root).unwrap();
    let url = envelope::open(&store, &[], &Default::default())
        .unwrap()
        .get(id)
        .unwrap()
        .unwrap()
        .mailbox_url;
    Locator::new(store).locate(id, &url).unwrap().unwrap().emlx
}

#[test]
fn get_email_plain_full_shape() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    let e = email(&c, fx.ids.plain);
    assert_eq!(e.id, fx.ids.plain);
    assert_eq!(e.subject, "Quarterly numbers");
    assert_eq!(e.from.address, "alice@example.test");
    assert_eq!(e.from.name, "Alice Example");
    assert_eq!(e.to.len(), 1);
    assert_eq!(e.to[0].address, "paul@example.test");
    assert!(e.cc.is_empty() && e.bcc.is_empty());
    assert!(e.message_id.is_some());
    assert_eq!(e.mailbox, "INBOX");
    assert!(e.read);
    assert!(e.date_received.is_some() && e.date_sent.is_some());
    assert_eq!(
        e.body.trim_end(),
        "The quarterly numbers look solid this time.\nSee you on Monday."
    );
    assert_eq!(e.body_source, "plain");
    assert!(e.body_html.is_none());
    assert!(!e.truncated);
    assert!(!e.encryption.encrypted && !e.encryption.decrypted);
    assert_eq!(e.encryption.signature, "none");
    assert!(e.attachments.is_empty());

    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["body_source"], "plain");
    assert_eq!(json["encryption"]["encrypted"], false);

    // Recipients come from the index: 2 To, 1 Cc, 1 Bcc.
    let cc = email(&c, fx.ids.cc_target);
    assert_eq!(cc.to.len(), 2);
    assert_eq!(cc.cc.len(), 1);
    assert_eq!(cc.bcc.len(), 1);
    assert_eq!(cc.cc[0].address, "carol@example.test");
    assert_eq!(cc.bcc[0].address, "dave@example.test");

    let short = read::get_email(&c, fx.ids.plain, 10, false).unwrap();
    assert_eq!(short.body, "The quarte");
    assert!(short.truncated);

    let html = read::get_email(&c, fx.ids.html_only, usize::MAX, true).unwrap();
    assert_eq!(html.body_source, "html");
    assert!(html.body_html.is_some());

    let pdf = email(&c, fx.ids.with_pdf);
    assert_eq!(pdf.attachments.len(), 1);
    assert_eq!(pdf.attachments[0].filename, "a.pdf");
}

#[test]
fn get_email_unknown_id_is_not_found() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    match read::get_email(&c, 999_999, usize::MAX, false) {
        Err(ReadError::NotFound(999_999)) => {}
        Err(e) => panic!("expected NotFound, got {e}"),
        Ok(_) => panic!("expected NotFound, got an email"),
    }
}

#[test]
fn get_email_signed_only_not_verified() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    let e = email(&c, fx.ids.signed_only);
    assert_eq!(e.body, "I hereby sign this statement.");
    assert!(!e.encryption.encrypted && !e.encryption.decrypted);
    assert!(e.encryption.method.is_none());
    assert!(e.encryption.signed);
    assert_eq!(e.encryption.signature, "not verified");
    assert!(e.encryption.error.is_none());
    assert!(
        e.attachments.is_empty(),
        "the signature is not an attachment"
    );
}

#[test]
fn get_email_encrypted_openssl() {
    let fx = fixture::build(Epoch::Unix);
    let (Some(keys), Some(id)) = (&fx.smime, fx.ids.encrypted) else {
        eprintln!("skipped: the fixture has no S/MIME message (no openssl)");
        return;
    };
    let c = read::open(with_openssl(config(&fx), &keys.key, &keys.cert)).unwrap();
    let e = email(&c, id);
    assert_eq!(e.subject, "sealed");
    assert!(e.body.contains("the sealed sentence"), "body: {:?}", e.body);
    assert_eq!(e.body_source, "plain");
    assert!(e.encryption.encrypted);
    assert!(e.encryption.decrypted);
    assert_eq!(e.encryption.method.as_deref(), Some("openssl"));
    assert!(e.encryption.signed);
    assert_eq!(e.encryption.signature, "not verified");
    assert!(e.encryption.error.is_none());
}

#[test]
fn get_email_opaque_signed_shows_the_signed_content() {
    let fx = fixture::build(Epoch::Unix);
    let (Some(keys), Some(id)) = (&fx.smime, fx.ids.opaque_signed) else {
        eprintln!("skipped: the fixture has no S/MIME message (no openssl)");
        return;
    };
    // The openssl decryptor, so that no test runs `security`; signed-data needs no key.
    let c = read::open(with_openssl(config(&fx), &keys.key, &keys.cert)).unwrap();
    let e = email(&c, id);
    assert_eq!(e.subject, "opaque");
    assert_eq!(e.body.trim_end(), "the opaque sentence");
    assert_eq!(e.body_source, "plain");
    assert_eq!(
        e.encryption,
        EncryptionInfo {
            encrypted: false,
            decrypted: false,
            method: None,
            signed: true,
            signature: "not verified".into(),
            error: None,
        }
    );
    let names: Vec<&str> = e.attachments.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(names, ["opaque.txt"], "smime.p7m is hidden");
    let t = read::attachment_text(&c, id, Some("opaque.txt"), None, 20_000).unwrap();
    assert_eq!(t.text.trim_end(), "opaque attachment text");

    // A decoder that fails: the outer message as it is, and the error.
    let mut cfg = with_openssl(config(&fx), &keys.key, &keys.cert);
    cfg.smime.openssl_bin = "/usr/bin/false".into();
    let c = read::open(cfg).unwrap();
    let e = email(&c, id);
    assert_eq!((e.body.as_str(), e.body_source), ("", "none"));
    assert!(e.encryption.signed && !e.encryption.encrypted && !e.encryption.decrypted);
    assert_eq!(e.encryption.signature, "not verified");
    let error = e.encryption.error.expect("the decoder's error");
    assert!(error.contains("exit status 1"), "{error}");
    let names: Vec<&str> = e.attachments.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(names, ["smime.p7m"]);
    let t = read::attachment_text(&c, id, None, Some(0), 20_000).unwrap();
    assert_eq!(t.filename, "smime.p7m");
}

#[test]
fn get_email_encrypted_failure_keeps_headers() {
    let fx = fixture::build(Epoch::Unix);
    let Some(id) = fx.ids.encrypted else {
        eprintln!("skipped: the fixture has no S/MIME message (no openssl)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new("openssl")
        .args(
            "req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=other -keyout other.key -out other.crt"
                .split_whitespace(),
        )
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let cfg = with_openssl(
        config(&fx),
        &dir.path().join("other.key"),
        &dir.path().join("other.crt"),
    );
    let c = read::open(cfg).unwrap();
    let e = email(&c, id);
    assert!(e.encryption.encrypted);
    assert!(!e.encryption.decrypted);
    assert_eq!(e.encryption.method.as_deref(), Some("openssl"));
    assert!(e.encryption.error.is_some());
    assert_eq!(e.body, "");
    assert_eq!(e.body_source, "none");
    assert!(e.attachments.is_empty(), "smime.p7m is not listed");
    assert_eq!(e.subject, "sealed");
    assert_eq!(e.from.address, "probe@example.test");
    assert_eq!(e.to.len(), 1);
    assert_eq!(e.to[0].address, "paul@example.test");
    assert!(e.message_id.is_some());
}

#[test]
fn search_body_finds_sentence_within_window() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    let r = read::search(&c, &filter(Scope::Body, &["QUARTERLY numbers", "monday"])).unwrap();
    assert_eq!(ids(&r), [fx.ids.plain]);
    assert!(r.scanned.unwrap() >= 1);
    assert!(r.candidates_total.unwrap() >= r.scanned.unwrap());
    assert_eq!(r.skipped, 0);
    assert!(!r.truncated);
    assert!(r.hint.is_none());
}

#[test]
fn search_body_default_window_90_days() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    let mut f = filter(Scope::Body, &["zymurgy"]);
    assert!(read::search(&c, &f).unwrap().hits.is_empty());
    f.after = Some(Utc::now() - Duration::days(300));
    assert_eq!(ids(&read::search(&c, &f).unwrap()), [fx.ids.old_unique]);
}

#[test]
fn search_body_window_ends_at_before() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    // `old_unique` is 200 days old: inside [before − 90 days, before), outside the last 90 days.
    let mut f = filter(Scope::Body, &["zymurgy"]);
    f.before = Some(Utc::now() - Duration::days(190));
    let r = read::search(&c, &f).unwrap();
    assert_eq!(ids(&r), [fx.ids.old_unique]);
    assert_eq!(r.candidates_total, Some(1));
    // An explicit `after` still wins.
    f.after = Some(Utc::now() - Duration::days(195));
    assert!(read::search(&c, &f).unwrap().hits.is_empty());
    // A `before` at the start of time scans from there, without overflow.
    f.after = None;
    f.before = Some(chrono::DateTime::<Utc>::MIN_UTC);
    assert!(read::search(&c, &f).unwrap().hits.is_empty());
}

#[test]
fn search_body_cap_marks_truncated() {
    let fx = fixture::build(Epoch::Unix);
    let f = filter(Scope::Body, &["no such phrase anywhere"]);

    let r = read::search(&ctx(&fx), &f).unwrap();
    assert!(!r.truncated);
    assert!(r.hint.is_none());

    let mut cfg = config(&fx);
    cfg.body_scan_max = 3;
    let r = read::search(&read::open(cfg).unwrap(), &f).unwrap();
    assert!(r.hits.is_empty());
    assert_eq!(r.candidates_total, Some(3));
    assert_eq!(r.scanned, Some(3));
    assert!(r.truncated);
    assert_eq!(r.hint.as_deref(), Some(NARROW));
}

#[test]
fn search_body_limit_and_offset() {
    let fx = fixture::build_with(
        Epoch::Unix,
        Options {
            messages: 5,
            ..Options::default()
        },
    );
    let c = ctx(&fx);
    let mut f = filter(Scope::Body, &["filler body"]);
    let all = ids(&read::search(&c, &f).unwrap());
    assert_eq!(all.len(), 5);

    f.limit = 2;
    f.offset = 1;
    let r = read::search(&c, &f).unwrap();
    assert_eq!(ids(&r), all[1..3]);
    assert!(r.truncated, "stopped before the last candidate");
    assert_eq!(r.hint.as_deref(), Some(MORE));
    assert!(r.scanned.unwrap() < r.candidates_total.unwrap());
}

#[test]
fn search_body_skips_encrypted() {
    let fx = fixture::build(Epoch::Unix);
    let Some(keys) = &fx.smime else {
        eprintln!("skipped: the fixture has no S/MIME message (no openssl)");
        return;
    };
    // A decryptor that could open it is configured; the scan still must not.
    let c = read::open(with_openssl(config(&fx), &keys.key, &keys.cert)).unwrap();
    let r = read::search(&c, &filter(Scope::Body, &["the sealed sentence"])).unwrap();
    assert!(r.hits.is_empty());
    assert_eq!(r.skipped, 0);
}

#[test]
fn search_hits_carry_flag_hints() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    let mut f = filter(Scope::All, &[]);
    f.limit = 100;
    let r = read::search(&c, &f).unwrap();
    let hit = |id: i64| {
        r.hits
            .iter()
            .find(|h| h.row.id == id)
            .unwrap_or_else(|| panic!("{id} not in hits"))
    };
    let plain = hit(fx.ids.plain);
    assert!(!plain.encrypted && !plain.signed && !plain.has_attachments);
    assert!(hit(fx.ids.signed_only).signed);
    assert!(hit(fx.ids.with_pdf).has_attachments);
    if let Some(id) = fx.ids.encrypted {
        let enc = hit(id);
        assert!(enc.encrypted);
        assert!(!enc.signed, "bit 23 is not set on the enveloped message");
    }
    assert!(!r.truncated);
    assert!(r.scanned.is_none() && r.candidates_total.is_none());

    let json = serde_json::to_value(plain).unwrap();
    assert_eq!(json["id"], fx.ids.plain);
    assert_eq!(json["encrypted"], false);
    for gone in ["mailbox_url", "flags", "size", "deleted", "bcc", "row"] {
        assert!(json.get(gone).is_none(), "{gone} is in the hit");
    }
}

#[test]
fn search_header_scope_truncated_at_limit() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    let mut f = filter(Scope::Subject, &["Project kickoff"]);
    f.limit = 2;
    let r = read::search(&c, &f).unwrap();
    assert_eq!(r.hits.len(), 2);
    assert!(r.truncated);
    f.limit = 3;
    assert!(read::search(&c, &f).unwrap().truncated);
    f.limit = 4;
    assert!(!read::search(&c, &f).unwrap().truncated);
}

#[test]
fn missing_file_is_file_missing_error() {
    let fx = fixture::build(Epoch::Unix);
    fs::remove_file(emlx_path(&fx, fx.ids.plain)).unwrap();
    let c = ctx(&fx);
    match read::get_email(&c, fx.ids.plain, usize::MAX, false) {
        Err(ReadError::FileMissing { id, mailbox }) => {
            assert_eq!(id, fx.ids.plain);
            assert_eq!(mailbox, "INBOX");
        }
        Err(e) => panic!("expected FileMissing, got {e}"),
        Ok(_) => panic!("expected FileMissing, got an email"),
    }
    // In a body scan the missing file is skipped, not fatal.
    let r = read::search(&c, &filter(Scope::Body, &["quarterly"])).unwrap();
    assert!(r.hits.is_empty());
    assert_eq!(r.skipped, 1);
}

#[test]
fn accounts_db_missing_or_unreadable_names_accounts_by_uuid() {
    let fx = fixture::build_with(
        Epoch::Unix,
        Options {
            url_form: UrlForm::MailboxUuid,
            ..Options::default()
        },
    );
    let names = |c: &Context| -> Vec<String> {
        let accounts = c.envelope().accounts().unwrap();
        accounts.into_iter().map(|a| a.identifier).collect()
    };
    assert_eq!(
        names(&ctx(&fx)),
        ["paul@example.test", "excluded@gmail.example"]
    );

    let garbage = fx.dir.path().join("garbage.sqlite");
    fs::write(&garbage, b"not a database").unwrap();
    for db in [fx.dir.path().join("no-such.sqlite"), garbage] {
        let mut cfg = config(&fx);
        cfg.accounts_db = db;
        let c = read::open(cfg).unwrap();
        assert_eq!(names(&c), [fixture::ACCOUNT_A, fixture::ACCOUNT_B]);
        assert_eq!(email(&c, fx.ids.plain).account, fixture::ACCOUNT_A);
    }
}
