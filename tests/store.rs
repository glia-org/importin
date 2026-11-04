mod support;

use importin::store::{self, StoreError};
use rusqlite::{Connection, OpenFlags};
use support::fixture::{self, Epoch};

#[test]
fn locates_highest_version_dir() {
    let fx = fixture::build(Epoch::Unix);
    assert!(fx.mail_root.join("V9").is_dir(), "decoy V9 missing");
    let s = store::locate(&fx.mail_root).unwrap();
    assert!(s.version_dir.ends_with("V10"), "{:?}", s.version_dir);
    assert!(s.envelope_index.ends_with("V10/MailData/Envelope Index"));
}

#[test]
fn missing_root_is_missing_or_denied() {
    let r = store::locate(std::path::Path::new("/nonexistent/Mail"));
    assert!(
        matches!(r, Err(StoreError::MissingOrDenied { .. })),
        "{r:?}"
    );
}

#[test]
fn unreadable_root_is_missing_or_denied() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Mail");
    std::fs::create_dir_all(root.join("V10")).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = store::locate(&root);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        matches!(r, Err(StoreError::MissingOrDenied { .. })),
        "{r:?}"
    );
}

#[test]
fn root_without_version_dir_is_no_version_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Vx")).unwrap();
    let r = store::locate(dir.path());
    assert!(matches!(r, Err(StoreError::NoVersionDir { .. })), "{r:?}");
}

#[test]
fn probe_reads_sqlite_header() {
    let fx = fixture::build(Epoch::Unix);
    let s = store::locate(&fx.mail_root).unwrap();
    let size = store::probe_access(&s).unwrap();
    assert!(size > 0);
    assert!(store::envelope_mtime(&s).is_some());
}

#[test]
fn fixture_has_expected_counts() {
    let fx = fixture::build(Epoch::Unix);
    let db = fx.mail_root.join("V10/MailData/Envelope Index");
    for sidecar in ["Envelope Index-wal", "Envelope Index-shm"] {
        assert!(db.with_file_name(sidecar).exists(), "{sidecar} missing");
    }
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert!(count("SELECT COUNT(*) FROM messages") >= 40);
    assert_eq!(count("SELECT COUNT(*) FROM mailboxes"), 5);
    let rcpts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM recipients WHERE message = ?",
            [fx.ids.cc_target],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rcpts, 4);
}

#[test]
fn cc_target_cc_and_bcc_appear_in_no_other_message() {
    let fx = fixture::build(Epoch::Unix);
    let db = fx.mail_root.join("V10/MailData/Envelope Index");
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for addr in ["carol@example.test", "dave@example.test"] {
        let others: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages m JOIN addresses a ON a.address = ?1 \
                 WHERE m.ROWID != ?2 AND (m.sender = a.ROWID OR EXISTS \
                 (SELECT 1 FROM recipients r WHERE r.message = m.ROWID AND r.address = a.ROWID))",
                rusqlite::params![addr, fx.ids.cc_target],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(others, 0, "{addr} appears outside cc_target");
    }
}
