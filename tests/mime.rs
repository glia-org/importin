mod support;

use std::fs;
use std::path::PathBuf;

use importin::emlx;
use importin::envelope::{self, Address};
use importin::mime::{self, AttachmentInfo, AttachmentState, BodySource, MimeError};
use importin::paths::Locator;
use importin::store;
use support::fixture::{self, Epoch, Fixture};

/// The raw RFC 822 bytes, the attachments dir and the emlx `date-received` of fixture message `id`.
struct Loaded {
    raw: Vec<u8>,
    dir: PathBuf,
    date_received: Option<i64>,
}

fn load(fx: &Fixture, id: i64) -> Loaded {
    let store = store::locate(&fx.mail_root).unwrap();
    let url = envelope::open(&store, &[])
        .unwrap()
        .get(id)
        .unwrap()
        .unwrap()
        .mailbox_url;
    let located = Locator::new(store).locate(id, &url).unwrap().unwrap();
    let emlx = emlx::read(&located.emlx).unwrap();
    Loaded {
        raw: emlx.raw,
        dir: located.attachments_dir,
        date_received: emlx.date_received,
    }
}

fn addr(name: &str, address: &str) -> Address {
    Address {
        name: name.into(),
        address: address.into(),
    }
}

fn info(part: &str, filename: &str, state: AttachmentState) -> AttachmentInfo {
    AttachmentInfo {
        index: 0,
        part: part.into(),
        filename: filename.into(),
        mime_type: "text/plain".into(),
        size: 0,
        state,
    }
}

#[test]
fn headers_all_address_lists() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.cc_target);
    let h = mime::parse(&m.raw).unwrap().headers();
    assert_eq!(h.from, [addr("Grace Example", "grace@example.test")]);
    assert_eq!(
        h.to,
        [
            addr("Paul Example", "paul@example.test"),
            addr("Bob Example", "bob@example.test")
        ]
    );
    assert_eq!(h.cc, [addr("Carol Example", "carol@example.test")]);
    assert_eq!(h.bcc, [addr("Dave Example", "dave@example.test")]);
    assert!(h.reply_to.is_empty());
    assert_eq!(h.subject, "Budget review meeting");
    assert_eq!(h.message_id.as_deref(), Some("115.fixture@example.test"));
    assert_eq!(h.content_type, "text/plain");
    assert_eq!(h.smime_type, None);
    assert_eq!(
        h.date.map(|d| d.timestamp()),
        m.date_received.map(|r| r - 120)
    );
}

#[test]
fn headers_threading_fields() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.thread[2]);
    let h = mime::parse(&m.raw).unwrap().headers();
    assert_eq!(h.in_reply_to.as_deref(), Some("kickoff-2@example.test"));
    assert_eq!(
        h.references,
        ["kickoff-1@example.test", "kickoff-2@example.test"]
    );
    assert_eq!(h.subject, "Re: Project kickoff");
}

#[test]
fn body_plain_preferred_over_html() {
    let raw = b"From: a@example.test\nContent-Type: multipart/alternative; boundary=\"alt\"\n\n\
--alt\nContent-Type: text/plain; charset=utf-8\n\nplain version\n\
--alt\nContent-Type: text/html; charset=utf-8\n\n<p>html version</p>\n\
--alt--\n";
    let parsed = mime::parse(raw).unwrap();
    let body = parsed.body(1000, false);
    assert_eq!(body.text.trim(), "plain version");
    assert_eq!(body.source, BodySource::Plain);
    assert_eq!(body.html, None);
    assert!(!body.truncated);
    let body = parsed.body(1000, true);
    assert_eq!(body.text.trim(), "plain version");
    assert!(body.html.unwrap().contains("<p>html version</p>"));
    assert!(parsed.attachments(None).is_empty());

    // A text/plain attachment is not the body: the HTML part is.
    let raw = b"From: a@example.test\nContent-Type: multipart/mixed; boundary=\"mix\"\n\n\
--mix\nContent-Type: text/html; charset=utf-8\n\n<p>the real body</p>\n\
--mix\nContent-Type: text/plain; name=\"notes.txt\"\nContent-Disposition: attachment; filename=\"notes.txt\"\n\nnotes\n\
--mix--\n";
    let parsed = mime::parse(raw).unwrap();
    let body = parsed.body(1000, false);
    assert_eq!(body.source, BodySource::Html);
    assert_eq!(body.text, "the real body");
    let atts = parsed.attachments(None);
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].filename, "notes.txt");

    // No text part at all.
    let raw = b"From: a@example.test\nContent-Type: application/octet-stream\n\nxyz\n";
    let body = mime::parse(raw).unwrap().body(1000, true);
    assert_eq!(body.source, BodySource::None);
    assert_eq!(body.text, "");
    assert_eq!(body.html, None);

    // The body of a forward is the outer text, not the text of the nested message.
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.rfc822_nested);
    let body = mime::parse(&m.raw).unwrap().body(1000, true);
    assert_eq!(
        body.text.trim(),
        "Forwarding the original announcement below."
    );
    assert_eq!(body.html, None);
}

#[test]
fn html_only_is_stripped() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.html_only);
    let parsed = mime::parse(&m.raw).unwrap();
    let body = parsed.body(10_000, false);
    assert_eq!(body.source, BodySource::Html);
    assert_eq!(body.html, None);
    assert!(!body.text.contains('<'), "{:?}", body.text);
    assert!(!body.text.contains("color"), "{:?}", body.text);
    assert_eq!(
        body.text,
        "First paragraph of the newsletter.\nSecond paragraph & more."
    );
    let with_html = parsed.body(10_000, true);
    assert!(with_html.html.unwrap().contains("<style>"));
}

#[test]
fn latin1_quoted_printable_decoded() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.latin1);
    let body = mime::parse(&m.raw).unwrap().body(1000, false);
    assert_eq!(body.source, BodySource::Plain);
    assert!(body.text.contains("Grüße aus München"), "{:?}", body.text);
}

#[test]
fn body_truncates_on_char_boundary() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.latin1);
    let parsed = mime::parse(&m.raw).unwrap();
    let body = parsed.body(5, false);
    assert_eq!(body.text, "Grüße");
    assert!(body.truncated);
    let body = parsed.body(usize::MAX, false);
    assert!(body.text.starts_with("Grüße aus München"));
    assert!(!body.truncated);
}

#[test]
fn attachment_states_four_kinds() {
    let fx = fixture::build(Epoch::Unix);

    let m = load(&fx, fx.ids.partial_external);
    let parsed = mime::parse(&m.raw).unwrap();
    let atts = parsed.attachments(Some(&m.dir));
    assert_eq!(atts.len(), 1);
    let a = &atts[0];
    assert_eq!(
        (
            a.index,
            a.part.as_str(),
            a.filename.as_str(),
            a.mime_type.as_str()
        ),
        (0, "2", "report.txt", "text/plain")
    );
    assert_eq!((a.state, a.size), (AttachmentState::External, 22));
    // Without the attachments dir the length header decides.
    assert_eq!(
        parsed.attachments(None)[0].state,
        AttachmentState::NotDownloaded
    );

    let m = load(&fx, fx.ids.not_downloaded);
    let parsed = mime::parse(&m.raw).unwrap();
    let atts = parsed.attachments(Some(&m.dir));
    assert_eq!(atts.len(), 1);
    assert_eq!(
        (atts[0].filename.as_str(), atts[0].mime_type.as_str()),
        ("slides.pdf", "application/pdf")
    );
    assert_eq!(
        (atts[0].state, atts[0].size),
        (AttachmentState::NotDownloaded, 1234)
    );
    assert!(matches!(
        parsed.attachment_bytes(&atts[0], Some(&m.dir)),
        Err(MimeError::Unavailable(AttachmentState::NotDownloaded))
    ));

    let m = load(&fx, fx.ids.not_extractable);
    let parsed = mime::parse(&m.raw).unwrap();
    let atts = parsed.attachments(Some(&m.dir));
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].filename, "ghost.bin");
    assert_eq!(
        (atts[0].state, atts[0].size),
        (AttachmentState::NotExtractable, 0)
    );
    assert!(matches!(
        parsed.attachment_bytes(&atts[0], Some(&m.dir)),
        Err(MimeError::Unavailable(AttachmentState::NotExtractable))
    ));

    let m = load(&fx, fx.ids.with_pdf);
    let parsed = mime::parse(&m.raw).unwrap();
    let atts = parsed.attachments(Some(&m.dir));
    assert_eq!(atts.len(), 1);
    assert_eq!(
        (atts[0].filename.as_str(), atts[0].mime_type.as_str()),
        ("a.pdf", "application/pdf")
    );
    assert_eq!(atts[0].state, AttachmentState::Inline);
    let bytes = parsed.attachment_bytes(&atts[0], Some(&m.dir)).unwrap();
    assert!(bytes.starts_with(b"%PDF"));
    assert_eq!(atts[0].size, bytes.len() as u64);
}

#[test]
fn attachment_part_numbering_nested() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.rfc822_nested);
    let parsed = mime::parse(&m.raw).unwrap();
    let atts = parsed.attachments(Some(&m.dir));
    assert_eq!(atts.len(), 1, "{atts:?}");
    let a = &atts[0];
    assert_eq!(
        (
            a.index,
            a.part.as_str(),
            a.filename.as_str(),
            a.mime_type.as_str()
        ),
        (0, "2", "original.eml", "message/rfc822")
    );
    assert_eq!(a.state, AttachmentState::Inline);
    let bytes = parsed.attachment_bytes(a, Some(&m.dir)).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("Subject: Original announcement"), "{text}");
    assert_eq!(a.size, text.len() as u64);

    // Multipart inside multipart: "2.2"; a nameless part gets a made-up unique name.
    let raw = b"From: a@example.test\nContent-Type: multipart/mixed; boundary=\"outer\"\n\n\
--outer\nContent-Type: text/plain\n\nouter text\n\
--outer\nContent-Type: multipart/mixed; boundary=\"inner\"\n\n\
--inner\nContent-Type: text/plain\n\ninner text\n\
--inner\nContent-Type: application/octet-stream; name=\"a.bin\"\nContent-Transfer-Encoding: base64\n\nAAEC\n\
--inner--\n\
--outer\nContent-Type: image/png\nContent-Transfer-Encoding: base64\n\niVBORw==\n\
--outer--\n";
    let parsed = mime::parse(raw).unwrap();
    let atts = parsed.attachments(None);
    let got: Vec<_> = atts
        .iter()
        .map(|a| {
            (
                a.index,
                a.part.as_str(),
                a.filename.as_str(),
                a.mime_type.as_str(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (0, "2.2", "a.bin", "application/octet-stream"),
            (1, "3", "part-3", "image/png"),
        ]
    );
    assert_eq!(
        parsed.attachment_bytes(&atts[0], None).unwrap(),
        [0u8, 1, 2]
    );

    // A single-part message whose body is the attachment is part "1".
    let raw = b"From: a@example.test\nContent-Type: application/pdf; name=\"x.pdf\"\n\n%PDF-1.4\n";
    let atts = mime::parse(raw).unwrap().attachments(None);
    assert_eq!(atts.len(), 1);
    assert_eq!(
        (atts[0].part.as_str(), atts[0].filename.as_str()),
        ("1", "x.pdf")
    );
}

#[test]
fn external_bytes_read_and_path_contained() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.partial_external);
    let parsed = mime::parse(&m.raw).unwrap();
    let atts = parsed.attachments(Some(&m.dir));
    assert_eq!(
        parsed.attachment_bytes(&atts[0], Some(&m.dir)).unwrap(),
        b"external report bytes\n"
    );

    // A filename that climbs out of the dir, onto a file that exists.
    fs::write(m.dir.parent().unwrap().join("x"), b"outside").unwrap();
    let crafted = info("2", "../../x", AttachmentState::External);
    assert!(matches!(
        parsed.attachment_bytes(&crafted, Some(&m.dir)),
        Err(MimeError::Unsafe(_))
    ));
    // A symlink inside the dir that points outside it.
    std::os::unix::fs::symlink(
        m.dir.parent().unwrap().join("x"),
        m.dir.join("2").join("link.txt"),
    )
    .unwrap();
    let crafted = info("2", "link.txt", AttachmentState::External);
    assert!(matches!(
        parsed.attachment_bytes(&crafted, Some(&m.dir)),
        Err(MimeError::Unsafe(_))
    ));

    // The file in another part dir is found by the filename scan.
    fs::rename(m.dir.join("2"), m.dir.join("9")).unwrap();
    let atts = parsed.attachments(Some(&m.dir));
    assert_eq!(atts[0].state, AttachmentState::External);
    assert_eq!(
        parsed.attachment_bytes(&atts[0], Some(&m.dir)).unwrap(),
        b"external report bytes\n"
    );
    // Dot dirs are not scanned.
    fs::rename(m.dir.join("9"), m.dir.join(".9")).unwrap();
    let atts = parsed.attachments(Some(&m.dir));
    assert_eq!(atts[0].state, AttachmentState::NotDownloaded);
}

#[test]
fn signed_only_hides_p7s() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.signed_only);
    let parsed = mime::parse(&m.raw).unwrap();
    assert!(parsed.is_signed());
    assert!(!parsed.is_enveloped());
    assert_eq!(parsed.enveloped_der(), None);
    assert!(parsed.attachments(Some(&m.dir)).is_empty());
    assert_eq!(
        parsed.body(1000, false).text.trim(),
        "I hereby sign this statement."
    );
    let first = String::from_utf8(parsed.signed_first_part().unwrap().to_vec()).unwrap();
    assert!(first.starts_with("Content-Type: text/plain"), "{first:?}");
    assert!(
        first.ends_with("I hereby sign this statement."),
        "{first:?}"
    );

    let m = load(&fx, fx.ids.plain);
    let parsed = mime::parse(&m.raw).unwrap();
    assert!(!parsed.is_signed());
    assert_eq!(parsed.signed_first_part(), None);
}

#[test]
fn enveloped_detected_and_der_decodes() {
    let fx = fixture::build(Epoch::Unix);
    let plain = load(&fx, fx.ids.plain);
    let parsed = mime::parse(&plain.raw).unwrap();
    assert!(!parsed.is_enveloped());
    assert_eq!(parsed.enveloped_der(), None);

    let Some(id) = fx.ids.encrypted else {
        eprintln!("skipped: the fixture has no encrypted message (no openssl)");
        return;
    };
    let m = load(&fx, id);
    let parsed = mime::parse(&m.raw).unwrap();
    assert!(parsed.is_enveloped());
    assert!(!parsed.is_auth_enveloped());
    assert!(!parsed.is_signed());
    let h = parsed.headers();
    assert_eq!(h.content_type, "application/pkcs7-mime");
    assert_eq!(h.smime_type.as_deref(), Some("enveloped-data"));
    assert_eq!(h.subject, "sealed");
    let der = parsed.enveloped_der().unwrap();
    assert_eq!(der[0], 0x30);
    assert!(der.len() > 100);

    // Named smime.p7m without an smime-type parameter.
    let raw = b"From: a@example.test\nContent-Type: application/pkcs7-mime; name=\"smime.p7m\"\n\
Content-Transfer-Encoding: base64\n\nMAMCAQA=\n";
    let parsed = mime::parse(raw).unwrap();
    assert!(parsed.is_enveloped());
    assert_eq!(
        parsed.enveloped_der().unwrap(),
        [0x30, 0x03, 0x02, 0x01, 0x00]
    );
    // authEnveloped-data.
    let raw = b"From: a@example.test\nContent-Type: application/pkcs7-mime; smime-type=authEnveloped-data; name=\"smime.p7m\"\n\
Content-Transfer-Encoding: base64\n\nMAMCAQA=\n";
    let parsed = mime::parse(raw).unwrap();
    assert!(parsed.is_enveloped());
    assert!(parsed.is_auth_enveloped());
    // signed-data is signed, not enveloped, although it is also named smime.p7m.
    let raw = b"From: a@example.test\nContent-Type: application/pkcs7-mime; smime-type=signed-data; name=\"smime.p7m\"\n\
Content-Transfer-Encoding: base64\n\nMAMCAQA=\n";
    let parsed = mime::parse(raw).unwrap();
    assert!(parsed.is_signed());
    assert!(!parsed.is_enveloped());
    assert_eq!(parsed.enveloped_der(), None);
}

#[test]
fn strip_html_basic() {
    use mime::strip_html;
    assert_eq!(
        strip_html("<p>a &amp; b</p><p>c&lt;d&gt;</p>"),
        "a & b\nc<d>"
    );
    assert_eq!(
        strip_html("<script type=\"x\">var x = '<p>';</script>keep<STYLE>p { }</STYLE>"),
        "keep"
    );
    assert_eq!(
        strip_html("one<br>two<BR/>three<br />four"),
        "one\ntwo\nthree\nfour"
    );
    assert_eq!(strip_html("<li>a</li><li>b</li>"), "a\nb");
    assert_eq!(
        strip_html("<h2>Title</h2><div>x</div><table><tr><td>c</td></tr></table>"),
        "Title\nx\nc"
    );
    assert_eq!(strip_html("&#228;&#xE4;&quot;&#39;&nbsp;x"), "ää\"' x");
    assert_eq!(
        strip_html("&amp;lt; &unknown; &#xZZ;"),
        "&lt; &unknown; &#xZZ;"
    );
    assert_eq!(strip_html("a  \t b"), "a b");
    assert_eq!(strip_html("x</p>\n\n\n\n<p>y"), "x\n\ny");
    assert_eq!(strip_html("  <p> x </p>  "), "x");
    assert_eq!(strip_html("a<!-- <p>hidden</p> -->b"), "ab");
    assert_eq!(strip_html("1 < 2 and 3 > 2"), "1 < 2 and 3 > 2");
}

#[test]
fn strip_html_decodes_named_entities() {
    use mime::strip_html;
    assert_eq!(strip_html("Gr&uuml;&szlig;e &euro; &rsquo;"), "Grüße € ’");
    assert_eq!(
        strip_html("&Auml;&Ouml;&Uuml;&auml;&ouml; &eacute;&egrave;&agrave;&ccedil;&ntilde;"),
        "ÄÖÜäö éèàçñ"
    );
    assert_eq!(
        strip_html(
            "&pound;&copy;&reg;&trade; a&ndash;b&mdash;c &lsquo;x&rsquo; &ldquo;y&rdquo;&hellip;"
        ),
        "£©®™ a–b—c ‘x’ “y”…"
    );
    assert_eq!(
        strip_html("&laquo;z&raquo; &bull; &middot; 5&deg; 2&times;3"),
        "«z» • · 5° 2×3"
    );
    // Single pass: a decoded `&` does not start another entity.
    assert_eq!(strip_html("&amp;uuml;"), "&uuml;");
}

#[test]
fn enveloped_only_at_the_top_level() {
    let raw = b"From: a@example.test\nContent-Type: multipart/mixed; boundary=\"mix\"\n\n\
--mix\nContent-Type: text/plain\n\nan encrypted file is attached\n\
--mix\nContent-Type: application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"\n\
Content-Disposition: attachment; filename=\"smime.p7m\"\nContent-Transfer-Encoding: base64\n\nMAMCAQA=\n\
--mix--\n";
    let parsed = mime::parse(raw).unwrap();
    assert!(!parsed.is_enveloped());
    assert!(!parsed.is_auth_enveloped());
    assert_eq!(parsed.enveloped_der(), None);
    let atts = parsed.attachments(None);
    assert_eq!(atts.len(), 1);
    assert_eq!(
        (
            atts[0].part.as_str(),
            atts[0].filename.as_str(),
            atts[0].mime_type.as_str()
        ),
        ("2", "smime.p7m", "application/pkcs7-mime")
    );
}

#[test]
fn body_truncates_html_too() {
    let raw = b"From: a@example.test\nContent-Type: multipart/alternative; boundary=\"alt\"\n\n\
--alt\nContent-Type: text/plain; charset=utf-8\n\nshort\n\
--alt\nContent-Type: text/html; charset=utf-8\n\n<p>Gr\xc3\xbc\xc3\x9fe</p>\n\
--alt--\n";
    let parsed = mime::parse(raw).unwrap();
    // The text fits, the HTML does not: cut on a char boundary, flagged.
    let body = parsed.body(6, true);
    assert_eq!(body.text.trim(), "short");
    assert_eq!(body.html.as_deref(), Some("<p>Grü"));
    assert!(body.truncated);
    // Without the HTML only the text counts.
    let body = parsed.body(6, false);
    assert_eq!(body.html, None);
    assert!(!body.truncated);
    // Both fit.
    let body = parsed.body(1000, true);
    assert_eq!(body.html.as_deref().map(str::trim), Some("<p>Grüße</p>"));
    assert!(!body.truncated);
}

#[test]
fn external_bytes_over_the_cap_are_refused() {
    let fx = fixture::build(Epoch::Unix);
    let m = load(&fx, fx.ids.partial_external);
    let parsed = mime::parse(&m.raw).unwrap();
    let max = emlx::MAX_EMLX_BYTES;
    fs::create_dir_all(m.dir.join("3")).unwrap();
    let big = fs::File::create(m.dir.join("3").join("big.bin")).unwrap();
    big.set_len(max + 1).unwrap(); // sparse: no disk blocks written
    let crafted = info("3", "big.bin", AttachmentState::External);
    match parsed.attachment_bytes(&crafted, Some(&m.dir)) {
        Err(MimeError::TooLarge { bytes, max: m }) => assert_eq!((bytes, m), (max + 1, max)),
        other => panic!("expected TooLarge, got {:?}", other.map(|b| b.len())),
    }
    // Exactly at the cap is still read.
    big.set_len(max).unwrap();
    assert_eq!(
        parsed
            .attachment_bytes(&crafted, Some(&m.dir))
            .unwrap()
            .len() as u64,
        max
    );
}
