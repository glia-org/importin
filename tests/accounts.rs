//! `accounts::load` on the fixture's synthetic Accounts database (never `~/Library/Accounts`).

mod support;

use importin::accounts::{self, AccountLabel};
use support::fixture::{self, ACCOUNT_A, ACCOUNT_B, Epoch};

#[test]
fn load_reads_username_description_and_type() {
    let fx = fixture::build(Epoch::Unix);
    let labels = accounts::load(&fx.accounts_db).unwrap();
    assert_eq!(labels.len(), 2);
    assert_eq!(
        labels[ACCOUNT_A],
        AccountLabel {
            uuid: ACCOUNT_A.into(),
            username: Some("paul@example.test".into()),
            description: Some("Example".into()),
            kind: Some("IMAP".into()),
        }
    );
    let b = &labels[ACCOUNT_B];
    assert_eq!(b.username.as_deref(), Some("excluded@gmail.example"));
    assert_eq!(
        (b.description.as_deref(), b.kind.as_deref()),
        (Some("Google"), Some("Google"))
    );
}

#[test]
fn load_uppercases_keys_and_keeps_nulls() {
    let fx = fixture::build(Epoch::Unix);
    // The fixture's own synthetic database, before `load` opens it.
    rusqlite::Connection::open(&fx.accounts_db)
        .unwrap()
        .execute_batch(
            "INSERT INTO ZACCOUNT (Z_PK, Z_ENT, Z_OPT, ZACCOUNTTYPE, ZIDENTIFIER) \
               VALUES (3, 1, 1, 99, 'c0ffee00-0000-4000-8000-000000000003');
             INSERT INTO ZACCOUNT (Z_PK, Z_ENT, Z_OPT) VALUES (4, 1, 1);",
        )
        .unwrap();
    let labels = accounts::load(&fx.accounts_db).unwrap();
    assert_eq!(labels.len(), 3, "a row without identifier is skipped");
    let c = &labels["C0FFEE00-0000-4000-8000-000000000003"];
    assert_eq!(c.uuid, "c0ffee00-0000-4000-8000-000000000003");
    assert_eq!(
        (&c.username, &c.description, &c.kind),
        (&None, &None, &None)
    );
}

#[test]
fn load_fails_on_a_missing_or_foreign_file_without_creating_one() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("Accounts4.sqlite");
    assert!(accounts::load(&missing).is_err());
    assert!(!missing.exists(), "read-only: nothing created");

    let garbage = dir.path().join("garbage.sqlite");
    std::fs::write(&garbage, b"not a database, just bytes").unwrap();
    assert!(accounts::load(&garbage).is_err());

    // An SQLite file without the Accounts tables.
    let fx = fixture::build(Epoch::Unix);
    let index = fx.mail_root.join("V10/MailData/Envelope Index");
    assert!(accounts::load(&index).is_err());
}

#[test]
fn default_path_is_the_users_accounts_database() {
    let p = accounts::default_path();
    assert!(p.ends_with("Library/Accounts/Accounts4.sqlite"), "{p:?}");
}
