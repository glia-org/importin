mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use importin::envelope;
use importin::paths::{self, Located, Locator, Resolution};
use importin::store::{self, Store};
use rusqlite::{Connection, OpenFlags};
use support::fixture::{self, ACCOUNT_A, ACCOUNT_B, Epoch, Fixture, Options, UrlForm};

fn store_of(fx: &Fixture) -> Store {
    store::locate(&fx.mail_root).unwrap()
}

fn url(fx: &Fixture, id: i64) -> String {
    let env = envelope::open(&store_of(fx), &[], &Default::default()).unwrap();
    env.get(id).unwrap().unwrap().mailbox_url
}

fn with_form(form: UrlForm) -> Fixture {
    fixture::build_with(
        Epoch::Unix,
        Options {
            url_form: form,
            ..Options::default()
        },
    )
}

fn locate(loc: &Locator, id: i64, url: &str) -> Located {
    loc.locate(id, url)
        .unwrap()
        .unwrap_or_else(|| panic!("{id} not found in {url}"))
}

fn attachments_of(emlx: &Path, id: i64) -> PathBuf {
    emlx.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("Attachments")
        .join(id.to_string())
}

/// Moves `id`'s emlx into the shard dir `shard` of the same `Data` tree; returns the new path.
fn move_to_shard(from: &Path, shard: &str) -> PathBuf {
    let data = from
        .ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "Data"))
        .unwrap();
    let dir = data.join(shard).join("Messages");
    fs::create_dir_all(&dir).unwrap();
    let to = dir.join(from.file_name().unwrap());
    fs::rename(from, &to).unwrap();
    to
}

/// Sets the mtime of the fixture's own file (never the real store) an hour away from now.
fn touch(path: &Path) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(3600))
        .unwrap();
}

#[test]
fn shard_formula() {
    assert_eq!(paths::shard(1_234_567), PathBuf::from("4/3/2/1"));
    assert_eq!(paths::shard(12_345), PathBuf::from("2/1"));
    assert_eq!(paths::shard(2001), PathBuf::from("2"));
    assert_eq!(paths::shard(1000), PathBuf::from("1"));
    assert_eq!(paths::shard(999), PathBuf::new());
    assert_eq!(paths::shard(0), PathBuf::new());
}

#[test]
fn locates_by_formula_both_url_forms() {
    for form in [UrlForm::ImapUserHost, UrlForm::MailboxUuid] {
        let fx = with_form(form);
        let id = fx.ids.plain;
        let u = url(&fx, id);
        let loc = Locator::new(store_of(&fx));
        let l = locate(&loc, id, &u);
        assert_eq!(l.by, Resolution::Formula, "{form:?}");
        assert!(
            l.emlx.ends_with(format!("Messages/{id}.emlx")),
            "{:?}",
            l.emlx
        );
        let mbox = fx.mail_root.join("V10").join(ACCOUNT_A).join("INBOX.mbox");
        assert!(l.emlx.starts_with(&mbox), "{:?}", l.emlx);
        assert!(l.emlx.is_file());
        assert_eq!(l.attachments_dir, attachments_of(&l.emlx, id));
    }
}

#[test]
fn locates_partial_emlx() {
    let fx = fixture::build(Epoch::Unix);
    let id = fx.ids.partial_external;
    let loc = Locator::new(store_of(&fx));
    let l = locate(&loc, id, &url(&fx, id));
    assert_eq!(l.by, Resolution::Formula);
    assert!(
        l.emlx.ends_with(format!("Messages/{id}.partial.emlx")),
        "{:?}",
        l.emlx
    );
    // The attachments dir is the sibling of `Messages` and holds the external part.
    let report = fs::read(l.attachments_dir.join("2").join("report.txt")).unwrap();
    assert_eq!(report, b"external report bytes\n");
}

#[test]
fn locates_big_rowid_four_digit_shard() {
    let fx = fixture::build(Epoch::Unix);
    let id = fx.ids.big_rowid;
    let loc = Locator::new(store_of(&fx));
    let l = locate(&loc, id, &url(&fx, id));
    assert_eq!(l.by, Resolution::Formula);
    assert!(
        l.emlx.ends_with("Data/4/3/2/1/Messages/1234567.emlx"),
        "{:?}",
        l.emlx
    );
    assert!(
        l.attachments_dir
            .ends_with("Data/4/3/2/1/Attachments/1234567")
    );
}

#[test]
fn umlaut_mailbox_dir_found() {
    for form in [UrlForm::ImapUserHost, UrlForm::MailboxUuid] {
        let fx = with_form(form);
        let id = fx.ids.umlaut_mailbox;
        let u = url(&fx, id);
        assert!(u.ends_with("/Gesendete%20Objekte%20%C3%84"), "{u}");
        let loc = Locator::new(store_of(&fx));
        let l = locate(&loc, id, &u);
        assert_eq!(l.by, Resolution::Formula, "{form:?}");
        let mbox = fx
            .mail_root
            .join("V10")
            .join(ACCOUNT_A)
            .join("Gesendete Objekte \u{c4}.mbox");
        // The directory on disk is NFD; the path built from the URL is NFC.
        assert!(l.emlx.starts_with(&mbox), "{:?}", l.emlx);
        assert!(l.emlx.is_file());
        let on_disk: Vec<String> = fs::read_dir(fx.mail_root.join("V10").join(ACCOUNT_A))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            on_disk.contains(&"Gesendete Objekte A\u{308}.mbox".to_string()),
            "{on_disk:?}"
        );
    }
}

/// Every message in the fixture's index resolves by formula: the fixture's own shard function and
/// directory layout agree with `paths` for every ROWID and both URL forms.
#[test]
fn every_fixture_message_resolves_by_formula() {
    for form in [UrlForm::ImapUserHost, UrlForm::MailboxUuid] {
        let fx = with_form(form);
        let s = store_of(&fx);
        let c = Connection::open_with_flags(&s.envelope_index, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        let mut q = c
            .prepare(
                "SELECT m.ROWID, b.url FROM messages m JOIN mailboxes b ON b.ROWID = m.mailbox",
            )
            .unwrap();
        let rows: Vec<(i64, String)> = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(rows.len() >= 40, "{}", rows.len());
        let loc = Locator::new(s);
        for (id, u) in rows {
            let l = locate(&loc, id, &u);
            assert_eq!(l.by, Resolution::Formula, "{id} {u}");
            let name = l.emlx.file_name().unwrap().to_str().unwrap().to_owned();
            assert!(
                name == format!("{id}.emlx") || name == format!("{id}.partial.emlx"),
                "{name}"
            );
        }
    }
}

#[test]
fn mailbox_dir_candidates_per_url_form() {
    let fx = fixture::build(Epoch::Unix);
    let s = store_of(&fx);
    let v10 = fx.mail_root.join("V10");
    let a = v10.join(ACCOUNT_A);
    let b = v10.join(ACCOUNT_B);

    // An account uuid names the one directory.
    let uuid_url = format!("mailbox://{ACCOUNT_A}/Archive/2025");
    assert_eq!(
        paths::mailbox_dir_candidates(&s, &uuid_url),
        vec![a.join("Archive.mbox").join("2025.mbox")]
    );

    // user@host: every account dir that holds the chain; INBOX exists in both accounts.
    let inbox = "imap://paul%40example.test@mail.example.test/INBOX";
    assert_eq!(
        paths::mailbox_dir_candidates(&s, inbox),
        vec![a.join("INBOX.mbox"), b.join("INBOX.mbox")]
    );
    let archive = "imap://paul%40example.test@mail.example.test/Archive/2025";
    assert_eq!(
        paths::mailbox_dir_candidates(&s, archive),
        vec![a.join("Archive.mbox").join("2025.mbox")]
    );
    let none = "imap://paul%40example.test@mail.example.test/Nowhere";
    assert!(paths::mailbox_dir_candidates(&s, none).is_empty());
}

#[test]
fn store_uuid_dir_is_the_36_char_child_with_data() {
    let fx = fixture::build(Epoch::Unix);
    let a = fx.mail_root.join("V10").join(ACCOUNT_A);
    let inbox = a.join("INBOX.mbox");
    let found = paths::store_uuid_dir(&inbox).unwrap();
    assert_eq!(found.parent().unwrap(), inbox);
    assert_eq!(found.file_name().unwrap().len(), 36);
    assert!(found.join("Data").is_dir());
    // `Archive.mbox` holds only the sub-mailbox `2025.mbox`, no store of its own.
    assert_eq!(paths::store_uuid_dir(&a.join("Archive.mbox")), None);
    assert!(paths::store_uuid_dir(&a.join("Archive.mbox").join("2025.mbox")).is_some());
}

#[test]
fn falls_back_to_walk_when_formula_misses() {
    let fx = fixture::build(Epoch::Unix);
    let id = fx.ids.plain;
    let u = url(&fx, id);
    let loc = Locator::new(store_of(&fx));
    let at_formula = locate(&loc, id, &u).emlx;
    let moved = move_to_shard(&at_formula, "9");

    let l = locate(&loc, id, &u);
    assert_eq!(l.by, Resolution::Walk);
    assert_eq!(l.emlx, moved);
    assert!(
        l.attachments_dir
            .ends_with(format!("Data/9/Attachments/{id}"))
    );
}

/// Each ROWID gets its `.emlx` in one wrong shard and a `.partial.emlx` copy in the other, the two
/// ROWIDs the other way round, so whichever shard the walk reaches first, one of them sees its
/// partial file first.
#[test]
fn walk_prefers_emlx_over_partial() {
    let fx = fixture::build(Epoch::Unix);
    let loc = Locator::new(store_of(&fx));
    for (id, full, partial) in [(fx.ids.plain, "9", "8"), (fx.ids.html_only, "8", "9")] {
        let u = url(&fx, id);
        let moved = move_to_shard(&locate(&loc, id, &u).emlx, full);
        let data = moved.ancestors().nth(3).unwrap();
        let dir = data.join(partial).join("Messages");
        fs::create_dir_all(&dir).unwrap();
        fs::copy(&moved, dir.join(format!("{id}.partial.emlx"))).unwrap();
    }
    let loc = Locator::new(store_of(&fx));
    for (id, full) in [(fx.ids.plain, "9"), (fx.ids.html_only, "8")] {
        let l = locate(&loc, id, &url(&fx, id));
        assert_eq!(l.by, Resolution::Walk);
        assert!(
            l.emlx.ends_with(format!("Data/{full}/Messages/{id}.emlx")),
            "{:?}",
            l.emlx
        );
    }
}

/// Caches the walk of INBOX through a miss for an absent ROWID, plants that ROWID's file in a wrong
/// shard, checks the cached miss still answers `None` (no walk per miss), then touches `stamp_file`
/// and checks the next lookup walks again and finds it.
fn assert_stamp_change_rewalks(fx: &Fixture, stamp_file: &Path) {
    let id = fx.ids.plain;
    let u = url(fx, id);
    let loc = Locator::new(store_of(fx));
    let at_formula = locate(&loc, id, &u).emlx;
    let data = at_formula.parent().unwrap().parent().unwrap().to_path_buf();
    assert!(data.ends_with("Data"));

    let absent = 999_999;
    assert!(loc.locate(absent, &u).unwrap().is_none());
    let dir = data.join("7").join("Messages");
    fs::create_dir_all(&dir).unwrap();
    let planted = dir.join(format!("{absent}.emlx"));
    fs::copy(&at_formula, &planted).unwrap();
    assert!(
        loc.locate(absent, &u).unwrap().is_none(),
        "a cached miss must not walk again"
    );

    touch(stamp_file);
    let l = locate(&loc, absent, &u);
    assert_eq!(l.by, Resolution::Walk);
    assert_eq!(l.emlx, planted);
}

#[test]
fn cache_invalidates_on_index_change() {
    let fx = fixture::build(Epoch::Unix);
    assert_stamp_change_rewalks(&fx, &store_of(&fx).envelope_index);
}

#[test]
fn cache_invalidates_on_wal_change() {
    let fx = fixture::build(Epoch::Unix);
    let mut wal = store_of(&fx).envelope_index.into_os_string();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    assert!(wal.is_file(), "fixture keeps its -wal");
    // A Mail write that sits only in the WAL changes the -wal mtime, not the main file's.
    assert_stamp_change_rewalks(&fx, &wal);
}

#[test]
fn cache_hit_with_missing_file_rewalks_once() {
    let fx = fixture::build(Epoch::Unix);
    let id = fx.ids.plain;
    let u = url(&fx, id);
    let loc = Locator::new(store_of(&fx));
    let first = move_to_shard(&locate(&loc, id, &u).emlx, "9");
    assert_eq!(locate(&loc, id, &u).emlx, first);

    // Moved again, no stamp change: the cached path is gone, so one walk finds the new one.
    let second = move_to_shard(&first, "8");
    let l = locate(&loc, id, &u);
    assert_eq!(l.by, Resolution::Walk);
    assert_eq!(l.emlx, second);

    // Gone from the tree: the cached path is missing, the one walk finds nothing, and its map
    // replaces the old entry...
    let parked = fx.dir.path().join("parked.emlx");
    fs::rename(&second, &parked).unwrap();
    assert!(loc.locate(id, &u).unwrap().is_none());
    // ...so a file appearing now is a cached miss, not another walk, until the stamp changes.
    let dir = second
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("7/Messages");
    fs::create_dir_all(&dir).unwrap();
    let third = dir.join(format!("{id}.emlx"));
    fs::rename(&parked, &third).unwrap();
    assert!(loc.locate(id, &u).unwrap().is_none());
    touch(&store_of(&fx).envelope_index);
    assert_eq!(locate(&loc, id, &u).emlx, third);
}
