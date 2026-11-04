//! Synthetic Apple Mail store for tests. It never touches the user's real Mail store; everything is
//! written under `Fixture::dir` (a temp dir removed on drop).
//!
//! Layout: `<dir>/Mail/V10/MailData/Envelope Index` (SQLite, WAL with persistent `-wal`/`-shm`),
//! a decoy `<dir>/Mail/V9/MailData/`, and per mailbox
//! `V10/<account-uuid>/<Seg>.mbox[/<Seg>.mbox]/<store-uuid>/Data/<shard>/Messages/<ROWID>.emlx`
//! (shard = digits of ROWID/1000 reversed, none below 1000; attachments in
//! `…/Data/<shard>/Attachments/<ROWID>/<part>/<filename>`). Each mailbox's own `.mbox` dir also
//! holds a decoy `Info.plist`. S/MIME key and cert live in `<dir>/smime/`.
//!
//! Mailboxes (ROWID: URL path, account): 1 `INBOX`, 2 `Sent Messages`, 3 `Gesendete Objekte`,
//! 4 `Archive/2025` (account A `paul@example.test`, host `mail.example.test`, dir `ACCOUNT_A`),
//! 5 `INBOX` (account B `excluded@gmail.example`, host `imap.gmail.example`, dir `ACCOUNT_B`).
//! `total_count`/`unread_count` count non-deleted rows.
//!
//! Dates: `base` = today 00:00 UTC; each message is some hours before it, so two builds on the same
//! UTC day agree on every date. Index timestamps follow `Epoch`; the emlx `date-received` is always
//! Unix seconds. `date_sent` = `date_received` − 120 s (also the `Date:` header). `messages.flags`
//! equals the emlx plist `flags` (bit 0 read, 1 deleted, 3 encrypted, 4 flagged, 10–15 attachment
//! count, 23 signed). `subject_prefix` is NULL unless shown. `conversation_id` = ROWID except the
//! thread. `message_id` = the `Message-ID` header without brackets. A `summaries` row (the body text)
//! exists for every single-part text message and for 102 and 103; multipart and encrypted ones have
//! none. Senders and recipients are written consistently to the headers and the index.
//!
//! | id | ROWID | mailbox | what it exercises |
//! |---|---|---|---|
//! | plain | 101 | INBOX | text/plain, read; Alice Example <alice@example.test> → Paul Example <paul@example.test>; body "The quarterly numbers look solid this time.\nSee you on Monday." |
//! | html_only | 102 | INBOX | only text/html (two `<p>`, a `<style>`, `&amp;`); sender newsletter@example.test, comment NULL; unread |
//! | latin1 | 103 | INBOX | iso-8859-1 quoted-printable `Gr=FC=DFe aus M=FCnchen` ("Grüße aus München") |
//! | partial_external | 104 | INBOX | `104.partial.emlx`; part 2 `report.txt` has an empty body and `X-Apple-Content-Length: 22`; bytes in `Attachments/104/2/report.txt` = "external report bytes\n" |
//! | not_downloaded | 105 | INBOX | part 2 `slides.pdf`: `X-Apple-Content-Length: 1234`, empty body, no file |
//! | not_extractable | 106 | INBOX | part 2 `ghost.bin`: attachment disposition, empty body, no length header, no file |
//! | single_dash | 107 | INBOX | payload ends `--b107-` (one dash short) directly before the plist |
//! | rfc822_nested | 108 | INBOX | `Fwd: ` prefix; multipart/mixed: 1 text, 2 message/rfc822 `original.eml` whose body is multipart/alternative (2.1 plain, 2.2 html) |
//! | signed_only | 109 | INBOX | multipart/signed: 1 text "I hereby sign this statement.", 2 fake `smime.p7s`; flag bit 23 |
//! | encrypted | 110 | INBOX | `None` without openssl. enveloped-data (aes256) over multipart/signed over multipart/alternative "the sealed sentence"; subject "sealed", from probe@example.test; flag bit 3; no summary |
//! | deleted | 111 | INBOX | `deleted = 1`, flag bit 1 |
//! | thread | 112, 113, 114 | INBOX, Sent Messages, INBOX | "Project kickoff", `Re: ` on 113 and 114, `conversation_id = 7`, oldest first, In-Reply-To/References chain |
//! | cc_target | 115 | INBOX | To Paul Example <paul@example.test>, Bob Example <bob@example.test>; Cc Carol Example <carol@example.test>; Bcc Dave Example <dave@example.test>; Carol and Dave appear in no other message |
//! | bcc_target | 116 | INBOX | To team@example.test (comment NULL), Bcc Paul |
//! | excluded | 117 | B INBOX | account B; B INBOX holds 117 and 129–131 |
//! | umlaut_mailbox | 118 | Gesendete Objekte | URL path `Gesendete%20Objekte`, dir `Gesendete Objekte.mbox`; To "Jörg Müller" (RFC 2047 in the header) |
//! | old_unique | 119 | INBOX | dated base − 200 days; body word "zymurgy" (in no other message) |
//! | big_rowid | 1_234_567 | INBOX | shard `4/3/2/1`; with `Options::huge_emlx` its emlx is a 65 MiB file (sparse, zero-padded) |
//! | with_pdf | 2001 | INBOX | inline base64 `a.pdf` (one page, Helvetica, "pdf sentence"); shard `2` |
//! | with_docx | 2002 | INBOX | inline base64 `a.docx` ("docx sentence") |
//! | with_xlsx | 12_345 | INBOX | inline base64 `a.xlsx` (inline-string cell A1 "xlsx sentence"); shard `2/1` |
//! | with_pptx | 2004 | INBOX | inline base64 `a.pptx` ("pptx sentence") |
//!
//! Rows without an id: 120 subject "axb" and 121 subject "a_b" (LIKE escaping); 122 sender
//! nocomment@example.test (comment NULL) and a To row with address NULL, comment "Undisclosed
//! recipients"; 123–126 `Archive/2025`, 40 days old; 127–128 Sent Messages; 129–131 B INBOX, unread;
//! 132 flagged + unread, 133 unread, 134 flagged, 135–136 read (INBOX); 137 `Re: ` in Gesendete
//! Objekte. Unread non-deleted rows: 102, 105–110, 114–117, 120–122, 129–133, 2001, 2002, 2004,
//! 12_345. Counts (no fillers, with `encrypted`): 42 messages; INBOX 28 total / 19 unread (the
//! deleted 111 not counted), Sent 3/0, Gesendete 2/0, Archive 4/0, B INBOX 4/4.
//! `Options::messages` adds plain read fillers from ROWID 100_000 in INBOX, subjects
//! "Filler message 0000"…, 7 days old and one minute older each.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rusqlite::{Connection, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Epoch {
    Unix,
    Cocoa,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlForm {
    /// `imap://paul%40example.test@mail.example.test/INBOX`
    ImapUserHost,
    /// `mailbox://<account-uuid>/INBOX`
    MailboxUuid,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub url_form: UrlForm,
    pub huge_emlx: bool,
    /// Extra plain filler messages in account A's INBOX.
    pub messages: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            url_form: UrlForm::ImapUserHost,
            huge_emlx: false,
            messages: 0,
        }
    }
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub mail_root: PathBuf,
    pub epoch: Epoch,
    pub ids: Ids,
    pub smime: Option<SmimeKeys>,
}

pub struct SmimeKeys {
    pub key: PathBuf,
    pub cert: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Ids {
    pub plain: i64,
    pub html_only: i64,
    pub latin1: i64,
    pub partial_external: i64,
    pub not_downloaded: i64,
    pub not_extractable: i64,
    pub single_dash: i64,
    pub rfc822_nested: i64,
    pub signed_only: i64,
    pub encrypted: Option<i64>,
    pub deleted: i64,
    pub thread: [i64; 3],
    pub cc_target: i64,
    pub bcc_target: i64,
    pub excluded: i64,
    pub umlaut_mailbox: i64,
    pub big_rowid: i64,
    pub with_pdf: i64,
    pub with_docx: i64,
    pub with_xlsx: i64,
    pub with_pptx: i64,
    pub old_unique: i64,
}

pub const ACCOUNT_A: &str = "4F3B2A10-8C1D-4E5F-9A6B-7C8D9E0F1A2B";
pub const ACCOUNT_B: &str = "B7E6D5C4-3A2B-4C1D-8E9F-0A1B2C3D4E5F";
const COCOA_OFFSET: i64 = 978_307_200;
const HUGE_BYTES: u64 = 65 * 1024 * 1024;
const BIG_ROWID: i64 = 1_234_567;

struct Mbx {
    account: &'static str,
    user: &'static str,
    host: &'static str,
    path: &'static [&'static str],
    store: &'static str,
}

const INBOX: usize = 0;
const SENT: usize = 1;
const GESENDET: usize = 2;
const ARCHIVE: usize = 3;
const B_INBOX: usize = 4;

const fn a_box(path: &'static [&'static str], store: &'static str) -> Mbx {
    Mbx {
        account: ACCOUNT_A,
        user: "paul@example.test",
        host: "mail.example.test",
        path,
        store,
    }
}

const MAILBOXES: [Mbx; 5] = [
    a_box(&["INBOX"], "0E1D2C3B-4A59-4687-9786-A5B4C3D2E1F0"),
    a_box(&["Sent Messages"], "1A2B3C4D-5E6F-4071-8293-A4B5C6D7E8F9"),
    a_box(
        &["Gesendete Objekte"],
        "2B3C4D5E-6F70-4182-93A4-B5C6D7E8F9A0",
    ),
    a_box(&["Archive", "2025"], "3C4D5E6F-7081-4293-A4B5-C6D7E8F9A0B1"),
    Mbx {
        account: ACCOUNT_B,
        user: "excluded@gmail.example",
        host: "imap.gmail.example",
        path: &["INBOX"],
        store: "4D5E6F70-8192-43A4-B5C6-D7E8F9A0B1C2",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
struct Addr {
    name: Option<&'static str>,
    addr: Option<&'static str>,
}

const fn ad(name: &'static str, addr: &'static str) -> Addr {
    Addr {
        name: Some(name),
        addr: Some(addr),
    }
}

const fn bare(addr: &'static str) -> Addr {
    Addr {
        name: None,
        addr: Some(addr),
    }
}

const PAUL: Addr = ad("Paul Example", "paul@example.test");
const ALICE: Addr = ad("Alice Example", "alice@example.test");
const BOB: Addr = ad("Bob Example", "bob@example.test");
const CAROL: Addr = ad("Carol Example", "carol@example.test");
const DAVE: Addr = ad("Dave Example", "dave@example.test");
const GRACE: Addr = ad("Grace Example", "grace@example.test");
const HANS: Addr = ad("Hans Huber", "hans@example.test");
const JOERG: Addr = ad("Jörg Müller", "joerg@example.test");
const TEAM: Addr = bare("team@example.test");
const NEWSLETTER: Addr = bare("newsletter@example.test");
const NOCOMMENT: Addr = bare("nocomment@example.test");
const UNDISCLOSED: Addr = Addr {
    name: Some("Undisclosed recipients"),
    addr: None,
};
const PROBE: Addr = bare("probe@example.test");
const EXCL: Addr = ad("Excluded Owner", "excluded@gmail.example");
const SOMEONE: Addr = ad("Someone Else", "someone@gmail.example");

const ENCRYPTED: u64 = 1 << 3;
const SIGNED: u64 = 1 << 23;

const fn att(n: u64) -> u64 {
    n << 10
}

struct Msg {
    rowid: i64,
    mb: usize,
    from: Addr,
    to: Vec<Addr>,
    cc: Vec<Addr>,
    bcc: Vec<Addr>,
    prefix: &'static str,
    subject: String,
    age_s: i64,
    read: bool,
    flagged: bool,
    deleted: bool,
    bits: u64,
    conv: i64,
    msgid: String,
    extra_headers: String,
    /// MIME headers and body after `MIME-Version: 1.0`.
    content: String,
    /// Complete message from openssl (headers included); replaces the generated headers.
    raw: Option<Vec<u8>>,
    summary: Option<String>,
    partial: bool,
    external: Vec<(u32, &'static str, Vec<u8>)>,
}

fn m(
    rowid: i64,
    mb: usize,
    from: Addr,
    to: &[Addr],
    subject: &str,
    age_h: i64,
    content: String,
) -> Msg {
    Msg {
        rowid,
        mb,
        from,
        to: to.to_vec(),
        cc: vec![],
        bcc: vec![],
        prefix: "",
        subject: subject.into(),
        age_s: age_h * 3600,
        read: false,
        flagged: false,
        deleted: false,
        bits: 0,
        conv: rowid,
        msgid: format!("{rowid}.fixture@example.test"),
        extra_headers: String::new(),
        content,
        raw: None,
        summary: None,
        partial: false,
        external: vec![],
    }
}

/// A text/plain message whose summary is its body.
fn simple(
    rowid: i64,
    mb: usize,
    from: Addr,
    to: &[Addr],
    subject: &str,
    age_h: i64,
    body: &str,
) -> Msg {
    Msg {
        summary: Some(body.into()),
        ..m(rowid, mb, from, to, subject, age_h, text(body) + "\n")
    }
}

pub fn build(epoch: Epoch) -> Fixture {
    build_with(epoch, Options::default())
}

pub fn build_with(epoch: Epoch, opts: Options) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let mail_root = dir.path().join("Mail");
    let v10 = mail_root.join("V10");
    fs::create_dir_all(v10.join("MailData")).unwrap();
    fs::create_dir_all(mail_root.join("V9").join("MailData")).unwrap();

    let smime = match make_smime(dir.path()) {
        Ok(s) => Some(s),
        Err(why) => {
            eprintln!("fixture: no S/MIME message: {why}");
            None
        }
    };

    let mut msgs = messages(smime.as_ref().map(|(_, raw)| raw.clone()));
    for i in 0..opts.messages {
        let body = format!("Filler body number {i}.");
        let filler = simple(
            100_000 + i as i64,
            INBOX,
            ALICE,
            &[PAUL],
            &format!("Filler message {i:04}"),
            0,
            &body,
        );
        msgs.push(Msg {
            age_s: 7 * 86_400 + i as i64 * 60,
            read: true,
            ..filler
        });
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let base = now - now % 86_400;

    for mbx in &MAILBOXES {
        let mbox = mbox_dir(&v10, mbx);
        fs::create_dir_all(mbox.join(mbx.store).join("Data")).unwrap();
        fs::write(mbox.join("Info.plist"), plist_doc("")).unwrap();
    }
    let mut sizes = Vec::with_capacity(msgs.len());
    for msg in &msgs {
        let raw = raw(msg, base);
        let mbx = &MAILBOXES[msg.mb];
        let data = mbox_dir(&v10, mbx)
            .join(mbx.store)
            .join("Data")
            .join(shard(msg.rowid));
        let messages_dir = data.join("Messages");
        fs::create_dir_all(&messages_dir).unwrap();
        let ext = if msg.partial { "partial.emlx" } else { "emlx" };
        let path = messages_dir.join(format!("{}.{ext}", msg.rowid));
        if opts.huge_emlx && msg.rowid == BIG_ROWID {
            let count = HUGE_BYTES - 9; // the prefix "68157431\n" is 9 bytes
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(format!("{count}\n").as_bytes()).unwrap();
            f.write_all(&raw).unwrap();
            f.set_len(HUGE_BYTES).unwrap();
            sizes.push(count as i64);
        } else {
            let mut bytes = format!("{}\n", raw.len()).into_bytes();
            bytes.extend_from_slice(&raw);
            bytes.extend_from_slice(emlx_plist(flags(msg), base - msg.age_s).as_bytes());
            fs::write(&path, bytes).unwrap();
            sizes.push(raw.len() as i64);
        }
        for (part, name, bytes) in &msg.external {
            let d = data
                .join("Attachments")
                .join(msg.rowid.to_string())
                .join(part.to_string());
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(name), bytes).unwrap();
        }
    }

    let index = v10.join("MailData").join("Envelope Index");
    write_index(&index, &msgs, &sizes, epoch, opts.url_form, base);

    Fixture {
        mail_root,
        epoch,
        ids: Ids {
            plain: 101,
            html_only: 102,
            latin1: 103,
            partial_external: 104,
            not_downloaded: 105,
            not_extractable: 106,
            single_dash: 107,
            rfc822_nested: 108,
            signed_only: 109,
            encrypted: smime.as_ref().map(|_| 110),
            deleted: 111,
            thread: [112, 113, 114],
            cc_target: 115,
            bcc_target: 116,
            excluded: 117,
            umlaut_mailbox: 118,
            big_rowid: BIG_ROWID,
            with_pdf: 2001,
            with_docx: 2002,
            with_xlsx: 12_345,
            with_pptx: 2004,
            old_unique: 119,
        },
        smime: smime.map(|(keys, _)| keys),
        dir,
    }
}

/// One row per message; the table at the top of this file says what each one is for.
#[rustfmt::skip]
fn messages(encrypted_raw: Option<Vec<u8>>) -> Vec<Msg> {
    let newsletter = html("<html><head><style>p { color: red; }</style></head><body>\
        <p>First paragraph of the newsletter.</p><p>Second paragraph &amp; more.</p></body></html>") + "\n";
    let latin1 = "Content-Type: text/plain; charset=iso-8859-1\nContent-Transfer-Encoding: quoted-printable\n\n\
        Gr=FC=DFe aus M=FCnchen\n".to_string();
    let external = multipart("mixed", "b104", &[
        text("Report attached, see report.txt."),
        attachment("text/plain", "report.txt", "X-Apple-Content-Length: 22\n", b""),
    ]);
    let not_downloaded = multipart("mixed", "b105", &[
        text("Slides are attached."),
        attachment("application/pdf", "slides.pdf", "X-Apple-Content-Length: 1234\n", b""),
    ]);
    let ghost = multipart("mixed", "b106", &[
        text("Nothing to see here."),
        attachment("application/octet-stream", "ghost.bin", "", b""),
    ]);
    let mut single_dash = multipart("mixed", "b107", &[text("The closing boundary of this message lost a dash.")]);
    single_dash.truncate(single_dash.len() - 2); // "--b107--\n" -> "--b107-"
    let original = multipart("alternative", "n108", &[
        text("The original announcement text."),
        html("<p>The original announcement text.</p>"),
    ]);
    let nested = format!(
        "Content-Type: message/rfc822\nContent-Disposition: attachment; filename=\"original.eml\"\n\n\
         From: {}\nTo: {}\nSubject: Original announcement\nDate: Mon, 3 Nov 2025 09:00:00 +0000\n\
         Message-ID: <original-108@example.test>\nMIME-Version: 1.0\n{}",
        hdr_addr(&BOB), hdr_addr(&ALICE), original.trim_end(),
    );
    let forward = multipart("mixed", "b108", &[text("Forwarding the original announcement below."), nested]);
    let p7s = format!(
        "Content-Type: application/pkcs7-signature; name=\"smime.p7s\"\nContent-Transfer-Encoding: base64\n\
         Content-Disposition: attachment; filename=\"smime.p7s\"\n\n{}",
        b64(b"\x30\x80 fake detached pkcs7 signature, not verifiable \x00\x00"),
    );
    let signed = multipart("signed; protocol=\"application/pkcs7-signature\"; micalg=sha-256", "b109", &[
        text("I hereby sign this statement."),
        p7s,
    ]);
    let mut v = vec![
        Msg { read: true, ..simple(101, INBOX, ALICE, &[PAUL], "Quarterly numbers", 2,
            "The quarterly numbers look solid this time.\nSee you on Monday.") },
        Msg { summary: Some("First paragraph of the newsletter. Second paragraph & more.".into()),
            ..m(102, INBOX, NEWSLETTER, &[PAUL], "Weekly newsletter", 5, newsletter) },
        Msg { read: true, summary: Some("Grüße aus München".into()),
            ..m(103, INBOX, HANS, &[PAUL], "Greetings from Munich", 8, latin1) },
        Msg { read: true, bits: att(1), partial: true, external: vec![(2, "report.txt", b"external report bytes\n".to_vec())],
            ..m(104, INBOX, BOB, &[PAUL], "Report attached", 11, external) },
        Msg { bits: att(1), ..m(105, INBOX, BOB, &[PAUL], "Slides for review", 14, not_downloaded) },
        Msg { bits: att(1), ..m(106, INBOX, BOB, &[PAUL], "Ghost attachment", 17, ghost) },
        m(107, INBOX, ALICE, &[PAUL], "Single dash boundary", 20, single_dash),
        Msg { prefix: "Fwd: ", bits: att(1), ..m(108, INBOX, ALICE, &[PAUL], "Original announcement", 23, forward) },
        Msg { bits: SIGNED, ..m(109, INBOX, ALICE, &[PAUL], "Signed statement", 26, signed) },
        Msg { read: true, deleted: true, ..simple(111, INBOX, ALICE, &[PAUL], "Deleted draft notes", 32, "These notes were deleted.") },
        Msg { read: true, conv: 7, msgid: "kickoff-1@example.test".into(),
            ..simple(112, INBOX, ALICE, &[PAUL], "Project kickoff", 60, "Shall we start the project on Monday?") },
        Msg { read: true, conv: 7, prefix: "Re: ", msgid: "kickoff-2@example.test".into(),
            extra_headers: "In-Reply-To: <kickoff-1@example.test>\nReferences: <kickoff-1@example.test>\n".into(),
            ..simple(113, SENT, PAUL, &[ALICE], "Project kickoff", 59, "Monday works for me.") },
        Msg { conv: 7, prefix: "Re: ", msgid: "kickoff-3@example.test".into(),
            extra_headers: "In-Reply-To: <kickoff-2@example.test>\n\
                            References: <kickoff-1@example.test> <kickoff-2@example.test>\n".into(),
            ..simple(114, INBOX, ALICE, &[PAUL], "Project kickoff", 58, "Great, Monday it is.") },
        Msg { cc: vec![CAROL], bcc: vec![DAVE],
            ..simple(115, INBOX, GRACE, &[PAUL, BOB], "Budget review meeting", 35, "Please join the budget review on Thursday.") },
        Msg { bcc: vec![PAUL], ..simple(116, INBOX, GRACE, &[TEAM], "Team offsite", 38, "The offsite is planned for next month.") },
        simple(117, B_INBOX, SOMEONE, &[EXCL], "Private note", 41, "This account is excluded."),
        Msg { read: true, ..simple(118, GESENDET, PAUL, &[JOERG], "Termin am Montag", 44, "Passt dir Montag um zehn?") },
        Msg { read: true, ..simple(119, INBOX, ALICE, &[PAUL], "Brewing notes", 200 * 24, "Notes on zymurgy and fermentation.") },
        simple(120, INBOX, BOB, &[PAUL], "axb", 47, "Subject without underscore."),
        simple(121, INBOX, BOB, &[PAUL], "a_b", 50, "Subject with an underscore."),
        simple(122, INBOX, NOCOMMENT, &[UNDISCLOSED], "Null address fields", 53, "Sender without a name, recipient without an address."),
        Msg { read: true, ..simple(123, ARCHIVE, ALICE, &[PAUL], "Archived item 1", 40 * 24, "Archived body one.") },
        Msg { read: true, ..simple(124, ARCHIVE, ALICE, &[PAUL], "Archived item 2", 40 * 24 + 1, "Archived body two.") },
        Msg { read: true, ..simple(125, ARCHIVE, ALICE, &[PAUL], "Archived item 3", 40 * 24 + 2, "Archived body three.") },
        Msg { read: true, ..simple(126, ARCHIVE, ALICE, &[PAUL], "Archived item 4", 40 * 24 + 3, "Archived body four.") },
        Msg { read: true, ..simple(127, SENT, PAUL, &[ALICE], "Lunch on Friday", 62, "Lunch on Friday at noon?") },
        Msg { read: true, ..simple(128, SENT, PAUL, &[BOB], "Draft agenda", 65, "Here is the draft agenda.") },
        simple(129, B_INBOX, SOMEONE, &[EXCL], "Gmail digest 1", 68, "Digest one."),
        simple(130, B_INBOX, SOMEONE, &[EXCL], "Gmail digest 2", 71, "Digest two."),
        simple(131, B_INBOX, SOMEONE, &[EXCL], "Gmail digest 3", 74, "Digest three."),
        Msg { flagged: true, ..simple(132, INBOX, CAROL, &[PAUL], "Action required: sign form", 77, "Please sign the form by Friday.") },
        simple(133, INBOX, DAVE, &[PAUL], "Coffee next week?", 80, "Coffee next week?"),
        Msg { read: true, flagged: true, ..simple(134, INBOX, ALICE, &[PAUL], "Travel itinerary", 83, "Your itinerary is below.") },
        Msg { read: true, ..simple(135, INBOX, BOB, &[PAUL], "Meeting minutes", 86, "Minutes of the last meeting.") },
        Msg { read: true, ..simple(136, INBOX, GRACE, &[PAUL], "Holiday schedule", 89, "The holiday schedule is out.") },
        Msg { read: true, prefix: "Re: ",
            ..simple(137, GESENDET, PAUL, &[HANS], "Greetings from Munich", 92, "Danke, viele Grüße zurück.") },
        Msg { read: true, ..simple(BIG_ROWID, INBOX, ALICE, &[PAUL], "Big rowid message", 95, "This message has a seven-digit ROWID.") },
    ];
    let office = [
        (2001, "PDF attached", "a.pdf", "application/pdf".to_string(), pdf()),
        (2002, "Word document attached", "a.docx", format!("{OOXML}.wordprocessingml.document"), docx()),
        (12_345, "Spreadsheet attached", "a.xlsx", format!("{OOXML}.spreadsheetml.sheet"), xlsx()),
        (2004, "Slides attached", "a.pptx", format!("{OOXML}.presentationml.presentation"), pptx()),
    ];
    for (i, (rowid, subject, name, mime, bytes)) in office.into_iter().enumerate() {
        let content = multipart("mixed", &format!("b{rowid}"), &[
            text(&format!("Attached: {name}.")),
            attachment(&mime, name, "", &bytes),
        ]);
        v.push(Msg { bits: att(1), ..m(rowid, INBOX, BOB, &[PAUL], subject, 98 + 3 * i as i64, content) });
    }
    if let Some(enc) = encrypted_raw {
        v.push(Msg { bits: ENCRYPTED, raw: Some(enc), ..m(110, INBOX, PROBE, &[PAUL], "sealed", 29, String::new()) });
    }
    v
}

fn flags(msg: &Msg) -> u64 {
    msg.read as u64 | (msg.deleted as u64) << 1 | (msg.flagged as u64) << 4 | msg.bits
}

fn raw(msg: &Msg, base: i64) -> Vec<u8> {
    let date = chrono::DateTime::from_timestamp(base - msg.age_s - 120, 0)
        .unwrap()
        .to_rfc2822();
    if let Some(enc) = &msg.raw {
        // openssl wrote To, From, Subject and the MIME headers; add the two it does not.
        let mut out = format!("Date: {date}\nMessage-ID: <{}>\n", msg.msgid).into_bytes();
        out.extend_from_slice(enc);
        return out;
    }
    let mut h = format!("From: {}\n", hdr_addr(&msg.from));
    for (name, list) in [("To", &msg.to), ("Cc", &msg.cc), ("Bcc", &msg.bcc)] {
        if !list.is_empty() {
            h += &format!(
                "{name}: {}\n",
                list.iter().map(hdr_addr).collect::<Vec<_>>().join(", ")
            );
        }
    }
    h += &format!(
        "Subject: {}{}\nDate: {date}\nMessage-ID: <{}>\n{}MIME-Version: 1.0\n{}",
        msg.prefix, msg.subject, msg.msgid, msg.extra_headers, msg.content
    );
    h.into_bytes()
}

fn write_index(path: &Path, msgs: &[Msg], sizes: &[i64], epoch: Epoch, form: UrlForm, base: i64) {
    let mut conn = Connection::open(path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    // Keep `-wal` and `-shm` after close, as on a Mac where Mail holds the database open.
    let mut on: std::ffi::c_int = 1;
    // SAFETY: open handle; "main" is NUL-terminated; SQLITE_FCNTL_PERSIST_WAL takes an int*.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&raw mut on).cast(),
        )
    };
    assert_eq!(rc, rusqlite::ffi::SQLITE_OK);
    conn.execute_batch(
        "CREATE TABLE messages (ROWID INTEGER PRIMARY KEY, message_id TEXT, global_message_id INTEGER, \
           sender INTEGER, subject INTEGER, subject_prefix TEXT, summary INTEGER, date_sent INTEGER, \
           date_received INTEGER, mailbox INTEGER, flags INTEGER, read INTEGER, flagged INTEGER, \
           deleted INTEGER, size INTEGER, conversation_id INTEGER);
         CREATE TABLE subjects (ROWID INTEGER PRIMARY KEY, subject TEXT);
         CREATE TABLE addresses (ROWID INTEGER PRIMARY KEY, address TEXT, comment TEXT);
         CREATE TABLE mailboxes (ROWID INTEGER PRIMARY KEY, url TEXT, total_count INTEGER, unread_count INTEGER);
         CREATE TABLE recipients (ROWID INTEGER PRIMARY KEY, message INTEGER, address INTEGER, type INTEGER, \
           position INTEGER);
         CREATE TABLE summaries (ROWID INTEGER PRIMARY KEY, summary TEXT);",
    )
    .unwrap();
    let ts = |unix: i64| match epoch {
        Epoch::Unix => unix,
        Epoch::Cocoa => unix - COCOA_OFFSET,
    };
    let tx = conn.transaction().unwrap();
    for (i, mbx) in MAILBOXES.iter().enumerate() {
        let live = || msgs.iter().filter(|m| m.mb == i && !m.deleted);
        tx.execute(
            "INSERT INTO mailboxes (ROWID, url, total_count, unread_count) VALUES (?, ?, ?, ?)",
            params![
                i as i64 + 1,
                url(mbx, form),
                live().count() as i64,
                live().filter(|m| !m.read).count() as i64
            ],
        )
        .unwrap();
    }
    let mut addresses: Vec<Addr> = Vec::new();
    let mut address_id = |a: Addr| -> i64 {
        if let Some(p) = addresses.iter().position(|x| *x == a) {
            return p as i64 + 1;
        }
        addresses.push(a);
        let id = addresses.len() as i64;
        let sql = "INSERT INTO addresses (ROWID, address, comment) VALUES (?, ?, ?)";
        tx.execute(sql, params![id, a.addr, a.name]).unwrap();
        id
    };
    let mut subjects: Vec<&str> = Vec::new();
    for (msg, size) in msgs.iter().zip(sizes) {
        let sender = address_id(msg.from);
        let subject = match subjects.iter().position(|s| *s == msg.subject) {
            Some(p) => p as i64 + 1,
            None => {
                subjects.push(&msg.subject);
                let id = subjects.len() as i64;
                let sql = "INSERT INTO subjects (ROWID, subject) VALUES (?, ?)";
                tx.execute(sql, params![id, msg.subject]).unwrap();
                id
            }
        };
        let summary = msg.summary.as_ref().map(|s| {
            tx.execute("INSERT INTO summaries (summary) VALUES (?)", [s])
                .unwrap();
            tx.last_insert_rowid()
        });
        let received = base - msg.age_s;
        tx.execute(
            "INSERT INTO messages (ROWID, message_id, global_message_id, sender, subject, subject_prefix, summary, \
               date_sent, date_received, mailbox, flags, read, flagged, deleted, size, conversation_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                msg.rowid,
                msg.msgid,
                msg.rowid + 500_000,
                sender,
                subject,
                (!msg.prefix.is_empty()).then_some(msg.prefix),
                summary,
                ts(received - 120),
                ts(received),
                msg.mb as i64 + 1,
                flags(msg) as i64,
                msg.read,
                msg.flagged,
                msg.deleted,
                size,
                msg.conv,
            ],
        )
        .unwrap();
        for (kind, list) in [(0, &msg.to), (1, &msg.cc), (2, &msg.bcc)] {
            for (position, a) in list.iter().enumerate() {
                let sql =
                    "INSERT INTO recipients (message, address, type, position) VALUES (?, ?, ?, ?)";
                tx.execute(
                    sql,
                    params![msg.rowid, address_id(*a), kind, position as i64],
                )
                .unwrap();
            }
        }
    }
    tx.commit().unwrap();
    conn.close().map_err(|(_, e)| e).unwrap();
}

fn url(mbx: &Mbx, form: UrlForm) -> String {
    let path = mbx
        .path
        .iter()
        .map(|s| pct(s))
        .collect::<Vec<_>>()
        .join("/");
    match form {
        UrlForm::ImapUserHost => format!("imap://{}@{}/{path}", pct(mbx.user), mbx.host),
        UrlForm::MailboxUuid => format!("mailbox://{}/{path}", mbx.account),
    }
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn mbox_dir(v10: &Path, mbx: &Mbx) -> PathBuf {
    let mut p = v10.join(mbx.account);
    for seg in mbx.path {
        p.push(format!("{seg}.mbox"));
    }
    p
}

fn shard(rowid: i64) -> PathBuf {
    match rowid / 1000 {
        0 => PathBuf::new(),
        n => n.to_string().chars().rev().map(String::from).collect(),
    }
}

fn plist_doc(dict: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n{dict}</dict>\n</plist>\n"
    )
}

fn emlx_plist(flags: u64, date_received: i64) -> String {
    plist_doc(&format!(
        "\t<key>date-received</key>\n\t<integer>{date_received}</integer>\n\
         \t<key>flags</key>\n\t<integer>{flags}</integer>\n"
    ))
}

fn hdr_addr(a: &Addr) -> String {
    match (a.name, a.addr) {
        (name, None) => format!("{}:;", name.unwrap_or("undisclosed-recipients")),
        (None, Some(x)) => x.into(),
        (Some(n), Some(x)) if n.is_ascii() => format!("{n} <{x}>"),
        (Some(n), Some(x)) => format!("{} <{x}>", rfc2047(n)),
    }
}

fn rfc2047(s: &str) -> String {
    let mut out = String::from("=?utf-8?q?");
    for b in s.bytes() {
        match b {
            b' ' => out.push('_'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(b as char),
            _ => out += &format!("={b:02X}"),
        }
    }
    out + "?="
}

// MIME part builders: headers, blank line, body without a trailing newline (the newline before a
// boundary belongs to the boundary).

fn text(body: &str) -> String {
    format!("Content-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: 7bit\n\n{body}")
}

fn html(body: &str) -> String {
    format!("Content-Type: text/html; charset=utf-8\n\n{body}")
}

fn attachment(mime: &str, name: &str, extra_headers: &str, bytes: &[u8]) -> String {
    format!(
        "Content-Type: {mime}; name=\"{name}\"\nContent-Disposition: attachment; filename=\"{name}\"\n\
         Content-Transfer-Encoding: base64\n{extra_headers}\n{}",
        b64(bytes)
    )
}

fn multipart(subtype: &str, boundary: &str, parts: &[String]) -> String {
    let mut s = format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"\n\n");
    for p in parts {
        s += &format!("--{boundary}\n{p}\n");
    }
    s + &format!("--{boundary}--\n")
}

fn b64(bytes: &[u8]) -> String {
    let s = base64::engine::general_purpose::STANDARD.encode(bytes);
    s.as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One page, Helvetica, uncompressed content stream showing "pdf sentence".
fn pdf() -> Vec<u8> {
    let stream = "BT /F1 24 Tf 72 720 Td (pdf sentence) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> \
         /Contents 4 0 R >>"
            .to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

const XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn content_types(overrides: &[(&str, &str)]) -> String {
    let o: String = overrides
        .iter()
        .map(|(p, t)| format!("<Override PartName=\"{p}\" ContentType=\"{t}\"/>"))
        .collect();
    format!(
        "{XML}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>{o}</Types>"
    )
}

fn rels(kind: &str, target: &str) -> String {
    format!(
        "{XML}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{REL_NS}/{kind}\" Target=\"{target}\"/></Relationships>"
    )
}

fn zip(entries: &[(&str, String)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in entries {
        w.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

const OOXML: &str = "application/vnd.openxmlformats-officedocument";

fn docx() -> Vec<u8> {
    zip(&[
        (
            "[Content_Types].xml",
            content_types(&[(
                "/word/document.xml",
                &format!("{OOXML}.wordprocessingml.document.main+xml"),
            )]),
        ),
        ("_rels/.rels", rels("officeDocument", "word/document.xml")),
        (
            "word/document.xml",
            format!(
                "{XML}<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
                 <w:body><w:p><w:r><w:t>docx sentence</w:t></w:r></w:p></w:body></w:document>"
            ),
        ),
    ])
}

fn xlsx() -> Vec<u8> {
    let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    zip(&[
        (
            "[Content_Types].xml",
            content_types(&[
                (
                    "/xl/workbook.xml",
                    &format!("{OOXML}.spreadsheetml.sheet.main+xml"),
                ),
                (
                    "/xl/worksheets/sheet1.xml",
                    &format!("{OOXML}.spreadsheetml.worksheet+xml"),
                ),
            ]),
        ),
        ("_rels/.rels", rels("officeDocument", "xl/workbook.xml")),
        (
            "xl/workbook.xml",
            format!(
                "{XML}<workbook xmlns=\"{ns}\" xmlns:r=\"{REL_NS}\">\
                 <sheets><sheet name=\"Sheet1\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
            ),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            rels("worksheet", "worksheets/sheet1.xml"),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(
                "{XML}<worksheet xmlns=\"{ns}\"><sheetData><row r=\"1\">\
                 <c r=\"A1\" t=\"inlineStr\"><is><t>xlsx sentence</t></is></c></row></sheetData></worksheet>"
            ),
        ),
    ])
}

fn pptx() -> Vec<u8> {
    let p = "http://schemas.openxmlformats.org/presentationml/2006/main";
    let a = "http://schemas.openxmlformats.org/drawingml/2006/main";
    zip(&[
        (
            "[Content_Types].xml",
            content_types(&[
                (
                    "/ppt/presentation.xml",
                    &format!("{OOXML}.presentationml.presentation.main+xml"),
                ),
                (
                    "/ppt/slides/slide1.xml",
                    &format!("{OOXML}.presentationml.slide+xml"),
                ),
            ]),
        ),
        (
            "_rels/.rels",
            rels("officeDocument", "ppt/presentation.xml"),
        ),
        (
            "ppt/presentation.xml",
            format!(
                "{XML}<p:presentation xmlns:p=\"{p}\" xmlns:r=\"{REL_NS}\">\
                 <p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/></p:sldIdLst></p:presentation>"
            ),
        ),
        (
            "ppt/_rels/presentation.xml.rels",
            rels("slide", "slides/slide1.xml"),
        ),
        (
            "ppt/slides/slide1.xml",
            format!(
                "{XML}<p:sld xmlns:a=\"{a}\" xmlns:p=\"{p}\"><p:cSld><p:spTree><p:sp><p:txBody>\
                 <a:p><a:r><a:t>pptx sentence</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"
            ),
        ),
    ])
}

const INNER: &str = "Content-Type: multipart/alternative; boundary=\"sealed-alt\"\n\n\
--sealed-alt\nContent-Type: text/plain; charset=utf-8\n\nthe sealed sentence\n\
--sealed-alt\nContent-Type: text/html; charset=utf-8\n\n<p>the sealed sentence</p>\n\
--sealed-alt--\n";

/// Throwaway key and cert plus an enveloped(signed(INNER)) RFC 822 message, all made by `openssl`.
fn make_smime(dir: &Path) -> Result<(SmimeKeys, Vec<u8>), String> {
    let d = dir.join("smime");
    fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    // No argument contains a space, so each command line is split on whitespace.
    let run = |cmd: &str| -> Result<(), String> {
        let out = Command::new("openssl")
            .args(cmd.split_whitespace())
            .current_dir(&d)
            .output()
            .map_err(|e| format!("cannot run openssl: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("`openssl {cmd}` failed: {}", stderr.trim()))
    };
    run(
        "req -x509 -newkey rsa:2048 -nodes -days 30 -subj /CN=probe/emailAddress=probe@example.test \
         -addext extendedKeyUsage=emailProtection -addext keyUsage=digitalSignature,keyEncipherment \
         -keyout key.pem -out cert.pem",
    )?;
    fs::write(d.join("inner.eml"), INNER).map_err(|e| e.to_string())?;
    run("cms -sign -signer cert.pem -inkey key.pem -in inner.eml -out signed.eml")?;
    run(
        "cms -encrypt -aes256 -recip cert.pem -in signed.eml -from probe@example.test \
         -to paul@example.test -subject sealed -out enc.eml",
    )?;
    let enc = fs::read(d.join("enc.eml")).map_err(|e| e.to_string())?;
    let keys = SmimeKeys {
        key: d.join("key.pem"),
        cert: d.join("cert.pem"),
    };
    Ok((keys, enc))
}
