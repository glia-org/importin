mod support;

use std::collections::HashMap;
use std::time::Instant;

use chrono::Utc;
use importin::accounts::{self, AccountLabel};
use importin::envelope::{
    self, Address, Envelope, EnvelopeError, Filter, MessageRow, OpenMode, Scope, parse_mailbox_url,
};
use importin::store;
use support::fixture::{self, Epoch, Fixture, Options, UrlForm};

fn try_open(fx: &Fixture, exclude: &[&str]) -> Result<Envelope, EnvelopeError> {
    let s = store::locate(&fx.mail_root).unwrap();
    let ex: Vec<String> = exclude.iter().map(|p| p.to_string()).collect();
    envelope::open(&s, &ex, &Default::default())
}

fn open(fx: &Fixture, exclude: &[&str]) -> Envelope {
    try_open(fx, exclude).unwrap()
}

/// With the account labels of the fixture's Accounts database.
fn open_labelled(fx: &Fixture, exclude: &[&str]) -> Envelope {
    let s = store::locate(&fx.mail_root).unwrap();
    let ex: Vec<String> = exclude.iter().map(|p| p.to_string()).collect();
    envelope::open(&s, &ex, &accounts::load(&fx.accounts_db).unwrap()).unwrap()
}

fn uuid_fixture() -> Fixture {
    let opts = Options {
        url_form: UrlForm::MailboxUuid,
        ..Options::default()
    };
    fixture::build_with(Epoch::Unix, opts)
}

/// Writes to the fixture's own synthetic index (never the real store), before an `Envelope` opens it.
fn mutate(fx: &Fixture, sql: &str) {
    let path = fx.mail_root.join("V10/MailData/Envelope Index");
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(sql).unwrap();
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
        limit: 200,
        offset: 0,
    }
}

fn ids(rows: &[MessageRow]) -> Vec<i64> {
    rows.iter().map(|r| r.id).collect()
}

fn search(env: &Envelope, scope: Scope, terms: &[&str]) -> Vec<i64> {
    ids(&env.search(&filter(scope, terms)).unwrap())
}

fn addr(name: &str, address: &str) -> Address {
    Address {
        name: name.into(),
        address: address.into(),
    }
}

#[test]
fn opens_read_only_and_resolves_schema() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    assert_eq!(env.mode, OpenMode::ReadOnly);
    let s = env.schema();
    assert!(s.has_recipients);
    assert!(s.has_summaries);
    assert!(s.optional_missing.is_empty(), "{:?}", s.optional_missing);
    assert!(s.optional_found.contains(&"messages.subject_prefix"));
    assert!(s.optional_found.contains(&"addresses.comment"));
}

#[test]
fn missing_required_column_is_schema_error_with_dump() {
    let fx = fixture::build(Epoch::Unix);
    mutate(&fx, "ALTER TABLE mailboxes RENAME COLUMN url TO link;");
    match try_open(&fx, &[]) {
        Err(EnvelopeError::Schema { missing, dump }) => {
            assert_eq!(missing, vec!["mailboxes.url".to_string()]);
            assert!(dump.contains("CREATE TABLE mailboxes"), "{dump}");
            assert!(dump.contains("CREATE TABLE messages"), "{dump}");
        }
        Err(e) => panic!("wrong error: {e:?}"),
        Ok(_) => panic!("opened despite a missing required column"),
    }
}

#[test]
fn epoch_detected_unix_and_cocoa() {
    let ux = fixture::build(Epoch::Unix);
    let co = fixture::build(Epoch::Cocoa);
    let (eu, ec) = (open(&ux, &[]), open(&co, &[]));
    assert_eq!(eu.epoch_offset(), 0);
    assert_eq!(ec.epoch_offset(), 978_307_200);
    let a = eu.get(ux.ids.plain).unwrap().unwrap();
    let b = ec.get(co.ids.plain).unwrap().unwrap();
    let (ra, rb) = (a.date_received.unwrap(), b.date_received.unwrap());
    assert_eq!(ra.to_rfc3339(), rb.to_rfc3339());
    assert_eq!(ra.offset().local_minus_utc(), 0);
    assert_eq!(
        a.date_sent.unwrap().to_rfc3339(),
        b.date_sent.unwrap().to_rfc3339()
    );
    assert_eq!((ra - a.date_sent.unwrap()).num_seconds(), 120);
    // `plain` is 2 h before today 00:00 UTC.
    let age = Utc::now().signed_duration_since(ra).num_hours();
    assert!((2..=26).contains(&age), "age {age} h");
}

#[test]
fn get_returns_to_cc_bcc_in_order() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    let m = env.get(fx.ids.cc_target).unwrap().unwrap();
    assert_eq!(m.subject, "Budget review meeting");
    assert_eq!(m.from, addr("Grace Example", "grace@example.test"));
    let paul = addr("Paul Example", "paul@example.test");
    let bob = addr("Bob Example", "bob@example.test");
    assert_eq!(m.to, vec![paul.clone(), bob.clone()]);
    assert_eq!(m.cc, vec![addr("Carol Example", "carol@example.test")]);
    assert_eq!(m.bcc, vec![addr("Dave Example", "dave@example.test")]);
    assert_eq!(m.mailbox, "INBOX");
    assert_eq!(m.account, "paul@example.test");
    assert_eq!(
        m.mailbox_url,
        "imap://paul%40example.test@mail.example.test/INBOX"
    );
    assert!(!m.read && !m.deleted);
    assert!(m.snippet.unwrap().contains("budget review"));
    drop(env);

    // Order follows `position`, not insertion order.
    mutate(
        &fx,
        "UPDATE recipients SET position = 1 - position WHERE message = 115 AND type = 0;",
    );
    let env = open(&fx, &[]);
    let m = env.get(fx.ids.cc_target).unwrap().unwrap();
    assert_eq!(m.to, vec![bob, paul]);
}

#[test]
fn subject_prefix_joined() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    let [first, second, third] = fx.ids.thread;
    let rows = env.search(&filter(Scope::Subject, &["Re:"])).unwrap();
    let got = ids(&rows);
    assert!(got.contains(&second) && got.contains(&third), "{got:?}");
    assert!(got.contains(&137), "{got:?}");
    assert!(!got.contains(&first), "{got:?}");
    let r = rows.iter().find(|r| r.id == second).unwrap();
    assert_eq!(r.subject, "Re: Project kickoff");
    // A term spanning prefix and subject matches the joined string.
    assert_eq!(
        search(&env, Scope::Subject, &["Re: Project"]),
        vec![third, second]
    );
}

#[test]
fn search_scope_cc_finds_only_cc() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    let id = fx.ids.cc_target;
    assert_eq!(search(&env, Scope::Cc, &["carol@example.test"]), vec![id]);
    assert!(search(&env, Scope::To, &["carol@example.test"]).is_empty());
    assert_eq!(search(&env, Scope::Cc, &["Carol"]), vec![id]); // the comment
    assert!(search(&env, Scope::Cc, &["dave@example.test"]).is_empty()); // Bcc only
    assert!(search(&env, Scope::To, &["bob@example.test"]).contains(&id));
}

#[test]
fn participant_matches_sender_and_recipients() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    let got = search(&env, Scope::Participant, &["grace@example.test"]);
    assert!(got.contains(&fx.ids.cc_target) && got.contains(&fx.ids.bcc_target));
    assert_eq!(
        search(&env, Scope::Participant, &["dave@example.test"]),
        vec![fx.ids.cc_target]
    );
    assert_eq!(
        search(&env, Scope::Participant, &["Carol"]),
        vec![fx.ids.cc_target]
    );
    assert!(search(&env, Scope::From, &["dave@example.test"]).is_empty());
    let from = search(&env, Scope::From, &["Grace Example"]);
    assert!(from.contains(&fx.ids.cc_target) && from.contains(&fx.ids.bcc_target));
    // Terms are ANDed.
    assert_eq!(
        search(&env, Scope::Participant, &["grace", "carol"]),
        vec![fx.ids.cc_target]
    );
}

#[test]
fn scope_all_covers_subject_participants_and_summary() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    assert!(search(&env, Scope::Subject, &["zymurgy"]).is_empty());
    assert_eq!(
        search(&env, Scope::All, &["zymurgy"]),
        vec![fx.ids.old_unique]
    );
    assert_eq!(search(&env, Scope::All, &["carol"]), vec![fx.ids.cc_target]);
    assert_eq!(
        search(&env, Scope::All, &["Budget review"]),
        vec![fx.ids.cc_target]
    );
}

#[test]
fn deleted_hidden_unless_asked() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    assert!(search(&env, Scope::Subject, &["Deleted draft"]).is_empty());
    let mut f = filter(Scope::Subject, &["Deleted draft"]);
    f.include_deleted = true;
    let rows = env.search(&f).unwrap();
    assert_eq!(ids(&rows), vec![fx.ids.deleted]);
    assert!(rows[0].deleted);
    assert!(env.get(fx.ids.deleted).unwrap().unwrap().deleted);
}

#[test]
fn excluded_account_invisible() {
    let fx = fixture::build(Epoch::Unix);
    let all = open(&fx, &[]);
    let m = all.get(fx.ids.excluded).unwrap().unwrap();
    assert_eq!(m.account, "excluded@gmail.example");
    assert_eq!(all.accounts().unwrap().len(), 2);
    drop(all);

    let env = open(&fx, &["*@gmail.example"]);
    assert!(env.get(fx.ids.excluded).unwrap().is_none());
    assert!(env.thread(fx.ids.excluded, 50).unwrap().is_empty());
    let accounts = env.accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].identifier, "paul@example.test");
    assert_eq!(accounts[0].user, "paul@example.test");
    assert_eq!(accounts[0].host, "mail.example.test");
    assert_eq!(accounts[0].scheme, "imap");
    assert_eq!(accounts[0].mailboxes, 4);
    assert_eq!(env.mailboxes(None).unwrap().len(), 4);
    assert!(env.mailboxes(Some("gmail")).unwrap().is_empty());
    assert!(search(&env, Scope::Subject, &["Gmail digest"]).is_empty());
    let every = env.search(&filter(Scope::Body, &[])).unwrap();
    assert!(every.iter().all(|r| !r.account.contains("gmail")));
    let mut f = filter(Scope::Body, &[]);
    f.account = Some("gmail".into());
    assert!(env.search(&f).unwrap().is_empty());
    let c = env.count().unwrap();
    assert_eq!((c.accounts, c.mailboxes, c.excluded_messages), (1, 4, 4));
    drop(env);

    // mailbox:// URLs carry no address: the account uuid is matched instead.
    let opts = Options {
        url_form: UrlForm::MailboxUuid,
        ..Options::default()
    };
    let fx = fixture::build_with(Epoch::Unix, opts);
    let env = open(&fx, &[fixture::ACCOUNT_B]);
    assert!(env.get(fx.ids.excluded).unwrap().is_none());
    let accounts = env.accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].identifier, fixture::ACCOUNT_A);
}

#[test]
fn bad_exclude_pattern_is_an_error() {
    let fx = fixture::build(Epoch::Unix);
    assert!(matches!(
        try_open(&fx, &["[unclosed"]),
        Err(EnvelopeError::Pattern { .. })
    ));
}

#[test]
fn like_metachars_escaped() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    assert_eq!(search(&env, Scope::Subject, &["a_b"]), vec![121]);
    assert_eq!(search(&env, Scope::Subject, &["axb"]), vec![120]);
    assert!(search(&env, Scope::Subject, &["%"]).is_empty());
    assert!(search(&env, Scope::All, &["\\"]).is_empty());
}

#[test]
fn search_folds_unicode_case() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    // 133 is from "Erika MÜLLER" with subject "Kaffee in der Müllerstraße"; 118 goes to "Jörg Müller".
    assert_eq!(search(&env, Scope::From, &["müller"]), vec![133]);
    assert_eq!(search(&env, Scope::From, &["MÜLLER"]), vec![133]);
    assert_eq!(search(&env, Scope::Subject, &["MÜLLER"]), vec![133]);
    assert_eq!(
        search(&env, Scope::To, &["MÜLLER"]),
        vec![fx.ids.umlaut_mailbox]
    );
    assert_eq!(
        search(&env, Scope::All, &["müller"]),
        vec![fx.ids.umlaut_mailbox, 133]
    );
    // Lowercasing only, no full case folding: "STRASSE" does not match "straße".
    assert!(search(&env, Scope::Subject, &["STRASSE"]).is_empty());
}

#[test]
fn null_address_fields_are_empty_strings() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    let m = env.get(122).unwrap().unwrap();
    assert_eq!(m.from, addr("", "nocomment@example.test"));
    assert_eq!(m.to, vec![addr("Undisclosed recipients", "")]);
    let n = env.get(fx.ids.html_only).unwrap().unwrap();
    assert_eq!(n.from, addr("", "newsletter@example.test"));
}

#[test]
fn thread_by_conversation_id() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    for id in fx.ids.thread {
        assert_eq!(ids(&env.thread(id, 50).unwrap()), fx.ids.thread.to_vec());
    }
    // The newest `limit`, oldest first.
    assert_eq!(
        ids(&env.thread(fx.ids.thread[0], 2).unwrap()),
        fx.ids.thread[1..].to_vec()
    );
    assert_eq!(
        ids(&env.thread(fx.ids.plain, 50).unwrap()),
        vec![fx.ids.plain]
    );
    assert!(env.thread(999_999, 50).unwrap().is_empty());
}

#[test]
fn thread_without_conversation_id_returns_the_message() {
    let fx = fixture::build(Epoch::Unix);
    mutate(
        &fx,
        "UPDATE messages SET conversation_id = NULL WHERE ROWID = 112;",
    );
    let env = open(&fx, &[]);
    assert_eq!(ids(&env.thread(112, 50).unwrap()), vec![112]);
    drop(env);

    mutate(
        &fx,
        "ALTER TABLE messages RENAME COLUMN conversation_id TO conv;",
    );
    let env = open(&fx, &[]);
    assert!(
        env.schema()
            .optional_missing
            .contains(&"messages.conversation_id")
    );
    assert_eq!(ids(&env.thread(113, 50).unwrap()), vec![113]);
}

#[test]
fn mailboxes_counts_and_unread() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    let enc = fx.ids.encrypted.is_some() as i64;
    // The opaque signed message is read.
    let opq = fx.ids.opaque_signed.is_some() as i64;
    let mbs = env.mailboxes(None).unwrap();
    assert_eq!(mbs.len(), 5);
    let find = |name: &str, account: &str| {
        mbs.iter()
            .find(|m| m.name == name && m.account == account)
            .unwrap_or_else(|| panic!("{name} / {account} missing: {mbs:?}"))
    };
    let a = "paul@example.test";
    let inbox = find("INBOX", a);
    assert_eq!((inbox.messages, inbox.unread), (27 + enc + opq, 18 + enc));
    let sent = find("Sent Messages", a);
    assert_eq!((sent.messages, sent.unread), (3, 0));
    let gesendet = find("Gesendete Objekte \u{c4}", a);
    assert_eq!((gesendet.messages, gesendet.unread), (2, 0));
    assert!(gesendet.url.ends_with("/Gesendete%20Objekte%20%C3%84"));
    let archive = find("Archive/2025", a);
    assert_eq!((archive.messages, archive.unread), (4, 0));
    let b = find("INBOX", "excluded@gmail.example");
    assert_eq!((b.messages, b.unread), (4, 4));

    let only_b = env.mailboxes(Some("GMAIL")).unwrap();
    assert_eq!(only_b.len(), 1);
    assert_eq!(only_b[0].account, "excluded@gmail.example");

    let c = env.count().unwrap();
    assert_eq!(c.messages, 40 + enc + opq);
    assert_eq!((c.mailboxes, c.accounts, c.excluded_messages), (5, 2, 0));
    let accounts = env.accounts().unwrap();
    let pa = accounts.iter().find(|x| x.identifier == a).unwrap();
    assert_eq!((pa.mailboxes, pa.messages), (4, 36 + enc + opq));
}

#[test]
fn search_body_scope_ignores_terms_and_sorts_newest_first() {
    for epoch in [Epoch::Unix, Epoch::Cocoa] {
        let fx = fixture::build(epoch);
        let env = open(&fx, &[]);
        let mut f = filter(Scope::Body, &["no-such-word-anywhere"]);
        f.limit = 5;
        let rows = env.search(&f).unwrap();
        let top = ids(&rows);
        assert_eq!(top.len(), 5, "{epoch:?}");
        assert_eq!(top[0], fx.ids.plain, "{epoch:?}");
        assert_eq!(top[1], fx.ids.html_only, "{epoch:?}");
        assert!(
            rows.windows(2)
                .all(|w| w[0].date_received >= w[1].date_received)
        );

        f.offset = 2;
        f.limit = 2;
        assert_eq!(ids(&env.search(&f).unwrap()), top[2..4].to_vec());

        // The other filters still apply.
        let cut = rows[1].date_received.unwrap().with_timezone(&Utc);
        let mut f = filter(Scope::Body, &["ignored"]);
        f.after = Some(cut);
        assert_eq!(
            ids(&env.search(&f).unwrap()),
            top[..2].to_vec(),
            "{epoch:?}"
        );
        let mut f = filter(Scope::Body, &["ignored"]);
        f.before = Some(cut);
        assert_eq!(ids(&env.search(&f).unwrap())[0], top[2], "{epoch:?}");
        let mut f = filter(Scope::Body, &[]);
        f.unread = true;
        assert_eq!(ids(&env.search(&f).unwrap())[0], fx.ids.html_only);
        let mut f = filter(Scope::Body, &[]);
        f.flagged = true;
        assert_eq!(ids(&env.search(&f).unwrap()), vec![132, 134]);
        let mut f = filter(Scope::Body, &[]);
        f.mailbox = Some("archive".into());
        assert_eq!(ids(&env.search(&f).unwrap()), vec![123, 124, 125, 126]);
    }
}

#[test]
fn parse_mailbox_url_both_forms() {
    let p = parse_mailbox_url("imap://paul%40example.test@mail.example.test/INBOX");
    assert_eq!(p.scheme, "imap");
    assert_eq!(p.authority, "paul%40example.test@mail.example.test");
    assert_eq!(p.user, "paul@example.test");
    assert_eq!(p.host, "mail.example.test");
    assert_eq!(p.account_uuid, None);
    assert_eq!(p.identifier, "paul@example.test");
    assert_eq!(p.mailbox_name, "INBOX");

    let p = parse_mailbox_url("imap://paul%40example.test@mail.example.test/Gesendete%20Objekte");
    assert_eq!(p.mailbox_name, "Gesendete Objekte");

    let p = parse_mailbox_url("imap://paul@mail.example.test/INBOX");
    assert_eq!(p.identifier, "paul@mail.example.test");

    let p = parse_mailbox_url(&format!("mailbox://{}/Archive/2025", fixture::ACCOUNT_A));
    assert_eq!(p.scheme, "mailbox");
    assert_eq!(p.user, "");
    assert_eq!(p.account_uuid.as_deref(), Some(fixture::ACCOUNT_A));
    assert_eq!(p.identifier, fixture::ACCOUNT_A);
    assert_eq!(p.mailbox_name, "Archive/2025");

    let opts = Options {
        url_form: UrlForm::MailboxUuid,
        ..Options::default()
    };
    let fx = fixture::build_with(Epoch::Unix, opts);
    let env = open(&fx, &[]);
    let m = env.get(fx.ids.umlaut_mailbox).unwrap().unwrap();
    assert_eq!(m.mailbox, "Gesendete Objekte \u{c4}");
    assert_eq!(m.account, fixture::ACCOUNT_A);
    assert!(m.mailbox_url.starts_with("mailbox://"));
}

#[test]
fn newest_encrypted_ids_uses_flag_bit_3() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &["*@gmail.example"]);
    let only: Vec<i64> = fx.ids.encrypted.into_iter().collect();
    assert_eq!(env.newest_encrypted_ids(10).unwrap(), only);
    drop(env);

    // Newest first; deleted and excluded rows never count.
    mutate(
        &fx,
        "UPDATE messages SET flags = flags | 8 WHERE ROWID IN (101, 111, 117);",
    );
    let env = open(&fx, &["*@gmail.example"]);
    let mut want = vec![fx.ids.plain];
    want.extend(fx.ids.encrypted);
    assert_eq!(env.newest_encrypted_ids(10).unwrap(), want);
    assert_eq!(env.newest_encrypted_ids(1).unwrap(), vec![fx.ids.plain]);
}

#[test]
fn header_search_under_50ms() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    let budget_ms = if cfg!(debug_assertions) { 200 } else { 50 };
    let cases = [
        (Scope::All, "example"),
        (Scope::All, "no-such-word-anywhere"),
        (Scope::Subject, "kickoff"),
        (Scope::From, "alice"),
        (Scope::To, "paul"),
        (Scope::Cc, "carol"),
        (Scope::Participant, "bob"),
    ];
    for (scope, term) in cases {
        let mut f = filter(scope, &[term]);
        f.limit = 25;
        let t = Instant::now();
        let rows = env.search(&f).unwrap();
        let ms = t.elapsed().as_millis();
        assert!(
            ms < budget_ms,
            "{scope:?} {term:?}: {ms} ms ({} rows)",
            rows.len()
        );
    }
}

#[test]
fn exclusion_sees_mailbox_added_after_open() {
    let fx = fixture::build(Epoch::Unix);
    let all = open(&fx, &[]);
    let env = open(&fx, &["*@gmail.example"]);
    // A mailbox of account B and a message in it, created while both are open.
    mutate(
        &fx,
        "INSERT INTO mailboxes (ROWID, url, total_count, unread_count) \
           VALUES (6, 'imap://excluded%40gmail.example@imap.gmail.example/Later', 1, 1);
         INSERT INTO subjects (ROWID, subject) VALUES (9000, 'Late arrival');
         INSERT INTO messages (ROWID, sender, subject, date_received, mailbox, flags, read, flagged, \
           deleted, conversation_id) \
           VALUES (5000, 1, 9000, CAST(strftime('%s', 'now') AS INTEGER) - 3600, 6, 8, 0, 0, 0, 5000);",
    );
    // The write reached the open readers.
    assert_eq!(search(&all, Scope::Subject, &["Late arrival"]), vec![5000]);
    assert_eq!(all.mailboxes(None).unwrap().len(), 6);

    assert!(search(&env, Scope::Subject, &["Late arrival"]).is_empty());
    assert!(env.get(5000).unwrap().is_none());
    assert!(!env.newest_encrypted_ids(50).unwrap().contains(&5000));
    let names: Vec<String> = env
        .mailboxes(None)
        .unwrap()
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names.len(), 4, "{names:?}");
    assert!(!names.contains(&"Later".to_string()));
    let c = env.count().unwrap();
    assert_eq!((c.mailboxes, c.accounts, c.excluded_messages), (4, 1, 5));
}

#[test]
fn snapshot_fallback_when_sidecars_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture::build(Epoch::Unix);
    let shm = fx.mail_root.join("V10/MailData/Envelope Index-shm");
    assert!(shm.exists(), "fixture keeps the -shm sidecar");
    let mode = std::fs::metadata(&shm).unwrap().permissions().mode();
    std::fs::set_permissions(&shm, std::fs::Permissions::from_mode(0o000)).unwrap();
    struct Restore<'a>(&'a std::path::Path, u32);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(self.1));
        }
    }
    let _restore = Restore(&shm, mode);
    if std::fs::File::open(&shm).is_ok() {
        eprintln!("skipped: a chmod 000 file is still readable (running as root?)");
        return;
    }
    let env = open(&fx, &[]);
    assert!(matches!(env.mode, OpenMode::Snapshot(_)), "{:?}", env.mode);
    let m = env.get(fx.ids.plain).unwrap().unwrap();
    assert_eq!(m.subject, "Quarterly numbers");
}

#[test]
fn exclude_globs_ignore_case_and_empty_components() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &["*@GMAIL.example"]);
    assert!(env.get(fx.ids.excluded).unwrap().is_none());
    assert_eq!(env.accounts().unwrap().len(), 1);
    drop(env);

    // uuid-form URLs have user "" and host "": `*@*` must not match "" or "@".
    let opts = Options {
        url_form: UrlForm::MailboxUuid,
        ..Options::default()
    };
    let fx = fixture::build_with(Epoch::Unix, opts);
    let env = open(&fx, &["*@*"]);
    assert_eq!(env.accounts().unwrap().len(), 2);
    assert!(env.get(fx.ids.excluded).unwrap().is_some());
    drop(env);
    let env = open(&fx, &[&fixture::ACCOUNT_B.to_lowercase()]);
    assert!(env.get(fx.ids.excluded).unwrap().is_none());
}

#[test]
fn from_filter_narrows_any_scope() {
    let fx = fixture::build(Epoch::Unix);
    let env = open(&fx, &[]);
    let alice = |rows: &[MessageRow]| {
        assert!(!rows.is_empty());
        assert!(
            rows.iter().all(|r| r.from.address == "alice@example.test"),
            "{:?}",
            ids(rows)
        );
    };
    let mut f = filter(Scope::All, &[]);
    f.from = Some("alice".into());
    let all = env.search(&f).unwrap();
    alice(&all);
    assert!(ids(&all).contains(&fx.ids.plain));
    let by_scope = search(&env, Scope::From, &["alice"]);
    assert_eq!(ids(&all), by_scope);

    let mut f = filter(Scope::Body, &["ignored"]);
    f.from = Some("Alice Example".into()); // the comment
    let body = env.search(&f).unwrap();
    alice(&body);
    assert_eq!(ids(&body), ids(&all));

    // Combined with terms in another scope.
    let mut f = filter(Scope::Subject, &["kickoff"]);
    f.from = Some("alice".into());
    assert_eq!(
        ids(&env.search(&f).unwrap()),
        vec![fx.ids.thread[2], fx.ids.thread[0]]
    );
    // LIKE-escaped.
    let mut f = filter(Scope::Body, &[]);
    f.from = Some("alice_example".into());
    assert!(env.search(&f).unwrap().is_empty());
}

#[test]
fn mailbox_uuid_accounts_are_named_by_their_labels() {
    let fx = uuid_fixture();
    let env = open_labelled(&fx, &[]);
    let accounts = env.accounts().unwrap();
    let names: Vec<&str> = accounts.iter().map(|a| a.identifier.as_str()).collect();
    assert_eq!(names, ["paul@example.test", "excluded@gmail.example"]);
    let a = &accounts[0];
    assert_eq!(a.label.as_deref(), Some("Example"));
    assert_eq!(a.kind.as_deref(), Some("IMAP"));
    assert_eq!(a.uuid.as_deref(), Some(fixture::ACCOUNT_A));
    assert_eq!((a.user.as_str(), a.host.as_str()), ("", ""));
    assert_eq!((a.mailboxes, a.scheme.as_str()), (4, "mailbox"));
    assert_eq!(accounts[1].label.as_deref(), Some("Google"));

    let m = env.get(fx.ids.plain).unwrap().unwrap();
    assert_eq!(m.account, "paul@example.test");
    assert_eq!(
        env.get(fx.ids.excluded).unwrap().unwrap().account,
        "excluded@gmail.example"
    );
    let boxes = env.mailboxes(Some("PAUL@")).unwrap();
    assert_eq!(boxes.len(), 4);
    assert!(boxes.iter().all(|b| b.account == "paul@example.test"));
    let mut f = filter(Scope::Body, &[]);
    f.account = Some("gmail".into());
    assert_eq!(ids(&env.search(&f).unwrap()), [117, 129, 130, 131]);

    // Without labels the same store names its accounts by uuid.
    let bare = open(&fx, &[]);
    let names: Vec<String> = bare
        .accounts()
        .unwrap()
        .into_iter()
        .map(|a| a.identifier)
        .collect();
    assert_eq!(names, [fixture::ACCOUNT_A, fixture::ACCOUNT_B]);
    assert!(
        bare.accounts()
            .unwrap()
            .iter()
            .all(|a| a.label.is_none() && a.kind.is_none())
    );
}

#[test]
fn exclusion_matches_label_username_description_and_kind() {
    let fx = uuid_fixture();
    for pattern in ["*@gmail.example", "Google", "google"] {
        let env = open_labelled(&fx, &[pattern]);
        assert!(env.get(fx.ids.excluded).unwrap().is_none(), "{pattern}");
        let accounts = env.accounts().unwrap();
        assert_eq!(accounts.len(), 1, "{pattern}");
        assert_eq!(accounts[0].identifier, "paul@example.test", "{pattern}");
        assert_eq!(env.count().unwrap().excluded_messages, 4, "{pattern}");
    }
    // The account type: every IMAP account.
    let env = open_labelled(&fx, &["imap"]);
    assert!(env.get(fx.ids.plain).unwrap().is_none());
    assert!(env.get(fx.ids.excluded).unwrap().is_some());
    // No label, nothing but the uuid to match.
    let env = open(&fx, &["Google"]);
    assert!(env.get(fx.ids.excluded).unwrap().is_some());
}

#[test]
fn identifier_falls_back_to_description_then_uuid() {
    let fx = uuid_fixture();
    let label = |uuid: &str, username: Option<&str>, description: Option<&str>| AccountLabel {
        uuid: uuid.into(),
        username: username.map(Into::into),
        description: description.map(Into::into),
        kind: None,
    };
    // Username without `@`: the description; neither: the uuid.
    let labels = HashMap::from([
        (
            fixture::ACCOUNT_A.to_string(),
            label(fixture::ACCOUNT_A, Some("paul"), Some("Work")),
        ),
        (
            fixture::ACCOUNT_B.to_string(),
            label(fixture::ACCOUNT_B, None, None),
        ),
    ]);
    let s = store::locate(&fx.mail_root).unwrap();
    let env = envelope::open(&s, &[], &labels).unwrap();
    let names: Vec<String> = env
        .accounts()
        .unwrap()
        .into_iter()
        .map(|a| a.identifier)
        .collect();
    assert_eq!(names, ["Work", fixture::ACCOUNT_B]);
}
