mod support;

use std::path::PathBuf;

use importin::emlx::{self, EmlxError, Flags, MAX_EMLX_BYTES};
use importin::paths::Locator;
use importin::store::{self, Store};
use rusqlite::{Connection, OpenFlags};
use support::fixture::{self, Epoch, Fixture, Options};

fn store_of(fx: &Fixture) -> Store {
    store::locate(&fx.mail_root).unwrap()
}

/// (ROWID, mailbox URL, index flags, index date_received) for every message of the fixture.
fn index_rows(s: &Store) -> Vec<(i64, String, i64, i64)> {
    let c =
        Connection::open_with_flags(&s.envelope_index, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut q = c
        .prepare(
            "SELECT m.ROWID, b.url, m.flags, m.date_received FROM messages m \
             JOIN mailboxes b ON b.ROWID = m.mailbox",
        )
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn emlx_path(fx: &Fixture, id: i64) -> PathBuf {
    let s = store_of(fx);
    let (_, url, _, _) = index_rows(&s).into_iter().find(|r| r.0 == id).unwrap();
    Locator::new(s).locate(id, &url).unwrap().unwrap().emlx
}

fn plist(flags: u64, date_received: i64) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>date-received</key>\n\t<integer>{date_received}</integer>\n\
         \t<key>flags</key>\n\t<integer>{flags}</integer>\n</dict>\n</plist>\n"
    )
}

#[test]
fn parses_count_raw_and_flags() {
    let fx = fixture::build(Epoch::Unix);
    let id = fx.ids.plain;
    let path = emlx_path(&fx, id);
    let bytes = std::fs::read(&path).unwrap();
    let count: usize =
        std::str::from_utf8(&bytes[..bytes.iter().position(|&b| b == b'\n').unwrap()])
            .unwrap()
            .parse()
            .unwrap();

    let e = emlx::read(&path).unwrap();
    assert_eq!(e.raw.len(), count);
    assert!(
        e.raw
            .starts_with(b"From: Alice Example <alice@example.test>\n")
    );
    assert!(
        e.raw.ends_with(b"See you on Monday.\n"),
        "{:?}",
        String::from_utf8_lossy(&e.raw)
    );
    assert!(e.flags.read());
    assert!(!e.flags.deleted());
    let (_, _, flags, received) = index_rows(&store_of(&fx))
        .into_iter()
        .find(|r| r.0 == id)
        .unwrap();
    assert_eq!(e.flags.0, flags as u64);
    assert_eq!(e.date_received, Some(received));
}

/// The trailer of every fixture emlx agrees with the index (the fixture writes both from one value).
#[test]
fn every_fixture_emlx_matches_index() {
    let fx = fixture::build(Epoch::Unix);
    let s = store_of(&fx);
    let rows = index_rows(&s);
    assert!(rows.len() >= 40);
    let loc = Locator::new(s);
    for (id, url, flags, received) in rows {
        let l = loc.locate(id, &url).unwrap().unwrap();
        let e = emlx::read(&l.emlx).unwrap_or_else(|err| panic!("{id}: {err}"));
        assert_eq!(e.flags.0, flags as u64, "{id}");
        assert_eq!(e.date_received, Some(received), "{id}");
        assert!(!e.raw.is_empty(), "{id}");
    }
}

#[test]
fn whitespace_terminated_count() {
    let body = "From: a@example.test\nSubject: x\n\nhello\n";
    for sep in [" ", "\t", "\r\n", "      \n", "\n"] {
        let bytes = format!("{}{sep}{body}{}", body.len(), plist(17, 1_700_000_000));
        let e = emlx::parse(bytes.as_bytes()).unwrap();
        assert_eq!(e.raw, body.as_bytes(), "{sep:?}");
        assert_eq!(e.flags.0, 17);
        assert_eq!(e.date_received, Some(1_700_000_000));
    }
    // Whitespace ends at the first newline: a payload that itself starts with a newline keeps it.
    let e = emlx::parse(b"3\n\nab").unwrap();
    assert_eq!(e.raw, b"\nab");
}

#[test]
fn rejects_bad_prefix_and_truncation() {
    for bad in [&b""[..], b"abc\nx", b"12", b"12x\n", b" 12\nab"] {
        assert!(
            matches!(emlx::parse(bad), Err(EmlxError::BadPrefix)),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    assert!(matches!(
        emlx::parse(b"100\nshort"),
        Err(EmlxError::Truncated)
    ));
}

#[test]
fn tolerates_missing_or_malformed_trailer() {
    for bytes in [
        &b"5\nhello"[..],
        b"5\nhello<?xml this is not a plist",
        b"5\nhellotrailing junk",
    ] {
        let e = emlx::parse(bytes).unwrap();
        assert_eq!(e.raw, b"hello");
        assert_eq!(e.flags.0, 0);
        assert_eq!(e.date_received, None);
    }
}

#[test]
fn repairs_single_dash_boundary() {
    let fx = fixture::build(Epoch::Unix);
    let path = emlx_path(&fx, fx.ids.single_dash);
    let on_disk = std::fs::read(&path).unwrap();
    let at = on_disk.windows(5).position(|w| w == b"<?xml").unwrap();
    assert!(
        on_disk[..at].ends_with(b"\n--b107-"),
        "fixture lost its single dash"
    );

    let count = at - (on_disk.iter().position(|&b| b == b'\n').unwrap() + 1);

    let e = emlx::read(&path).unwrap();
    assert!(
        e.raw.ends_with(b"\n--b107--"),
        "{:?}",
        String::from_utf8_lossy(&e.raw)
    );
    assert_eq!(e.raw.len(), count + 1);
    assert!(
        e.date_received.is_some(),
        "trailer still parsed after the repair"
    );
}

#[test]
fn leaves_proper_or_unfollowed_endings_alone() {
    let p = plist(0, 1);
    let cases = [
        ("x\n--b--", true), // already a closing boundary
        ("x\n--b-", false), // no plist follows: nothing to restore against
        ("x\nend-", true),  // not a boundary line
        ("x\n---", true),   // no token
    ];
    for (raw, with_plist) in cases {
        let trailer = if with_plist { p.as_str() } else { "" };
        let bytes = format!("{}\n{raw}{trailer}", raw.len());
        let e = emlx::parse(bytes.as_bytes()).unwrap();
        assert_eq!(e.raw, raw.as_bytes(), "{raw:?}");
    }
    let bytes = format!("6\nx\n--b-{p}");
    assert_eq!(emlx::parse(bytes.as_bytes()).unwrap().raw, b"x\n--b--");
}

#[test]
fn rejects_over_64mib() {
    let fx = fixture::build_with(
        Epoch::Unix,
        Options {
            huge_emlx: true,
            ..Options::default()
        },
    );
    let path = emlx_path(&fx, fx.ids.big_rowid);
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(size > MAX_EMLX_BYTES);
    match emlx::read(&path) {
        Err(EmlxError::TooLarge { bytes, max }) => {
            assert_eq!(bytes, size);
            assert_eq!(max, 64 * 1024 * 1024);
        }
        other => panic!("{:?}", other.map(|e| e.raw.len())),
    }
}

#[test]
fn flag_bits_layout() {
    let f = Flags(1 << 3 | 1 << 23 | 2 << 10);
    assert!(f.encrypted());
    assert!(f.signed());
    assert_eq!(f.attachment_count(), 2);
    assert!(!f.read() && !f.deleted() && !f.answered() && !f.flagged() && !f.draft() && !f.junk());

    assert!(Flags(1 << 0).read());
    assert!(Flags(1 << 1).deleted());
    assert!(Flags(1 << 2).answered());
    assert!(Flags(1 << 4).flagged());
    assert!(Flags(1 << 6).draft());
    assert!(Flags(1 << 24).junk());
    assert_eq!(Flags(0x3f << 10).attachment_count(), 63);
    assert_eq!(Flags(!(0x3f << 10)).attachment_count(), 0);
}
