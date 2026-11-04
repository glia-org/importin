mod support;

use std::collections::HashMap;
use std::process::Command;

use importin::attach;
use importin::config::{self, Config, DecryptorKind};
use importin::mime::{AttachmentState, MimeError};
use importin::read::{self, Context, ReadError};
use support::fixture::{self, Epoch, Fixture};

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

fn config(fx: &Fixture) -> Config {
    let env = HashMap::from([(
        "IMPORTIN_MAIL_ROOT".to_string(),
        fx.mail_root.display().to_string(),
    )]);
    config::parse("", &env).unwrap()
}

fn ctx(fx: &Fixture) -> Context {
    read::open(config(fx)).unwrap()
}

fn text(c: &Context, id: i64, filename: Option<&str>, index: Option<usize>) -> attach::Extracted {
    match read::attachment_text(c, id, filename, index, 20_000) {
        Ok(e) => e,
        Err(e) => panic!("attachment_text({id}, {filename:?}, {index:?}): {e}"),
    }
}

fn error(c: &Context, id: i64, filename: Option<&str>, index: Option<usize>) -> ReadError {
    read::attachment_text(c, id, filename, index, 20_000)
        .err()
        .expect("an error")
}

#[test]
fn pdf_text() {
    let e = attach::extract(&fixture::pdf(), "a.pdf", "application/pdf", 20_000);
    assert!(
        e.text.contains("pdf sentence"),
        "text: {:?}, note: {:?}",
        e.text,
        e.note
    );
    assert_eq!(e.note, None);
    assert!(!e.truncated);
    assert_eq!(e.filename, "a.pdf");
    assert_eq!(e.mime_type, "application/pdf");
}

#[test]
fn docx_text() {
    let e = attach::extract(&fixture::docx(), "a.docx", DOCX, 20_000);
    assert_eq!(e.text, "docx sentence\n", "note: {:?}", e.note);
    assert_eq!(e.note, None);
}

#[test]
fn xlsx_text() {
    let e = attach::extract(&fixture::xlsx(), "a.xlsx", XLSX, 20_000);
    assert_eq!(e.text, "== Sheet1\nxlsx sentence\n", "note: {:?}", e.note);
    assert_eq!(e.note, None);
}

#[test]
fn pptx_text() {
    let e = attach::extract(&fixture::pptx(), "a.pptx", PPTX, 20_000);
    assert_eq!(e.text, "== slide 1\npptx sentence\n", "note: {:?}", e.note);
    assert_eq!(e.note, None);
}

#[test]
fn html_stripped() {
    let html =
        b"<html><style>p { color: red }</style><body><p>html &amp; sentence</p></body></html>";
    let e = attach::extract(html, "a.HTM", "text/html", 20_000);
    assert_eq!(e.text, "html & sentence");
    assert_eq!(e.note, None);
}

#[test]
fn kind_from_mime_when_extension_unknown() {
    let e = attach::extract(&fixture::docx(), "part-2", DOCX, 20_000);
    assert_eq!(e.text, "docx sentence\n", "note: {:?}", e.note);
    let e = attach::extract(b"plain words", "part-3", "text/plain", 20_000);
    assert_eq!(e.text, "plain words");
}

#[test]
fn image_gives_note_only() {
    let e = attach::extract(b"\x89PNG", "a.png", "image/png", 20_000);
    assert_eq!(e.text, "");
    assert!(!e.truncated);
    let note = e.note.expect("a note");
    assert!(note.contains("image/png"), "note: {note}");
    assert!(note.contains("4 bytes"), "note: {note}");
}

#[test]
fn truncates() {
    let e = attach::extract("äöüßxyz".as_bytes(), "a.txt", "text/plain", 4);
    assert_eq!(e.text, "äöüß");
    assert!(e.truncated);
}

#[test]
fn malformed_documents_give_notes() {
    for (name, mime, kind) in [
        ("a.pdf", "application/pdf", "pdf"),
        ("a.docx", DOCX, "docx"),
        ("a.xlsx", XLSX, "spreadsheet"),
        ("a.pptx", PPTX, "pptx"),
    ] {
        let e = attach::extract(b"definitely not a document", name, mime, 20_000);
        assert_eq!(e.text, "", "{name}");
        let note = e.note.unwrap_or_else(|| panic!("{name}: no note"));
        assert!(
            note.starts_with(&format!("{kind} extraction failed: ")),
            "{name}: {note}"
        );
    }
}

#[test]
fn read_attachment_text_end_to_end() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    let e = text(&c, fx.ids.with_pdf, Some("a.pdf"), None);
    assert!(e.text.contains("pdf sentence"), "text: {:?}", e.text);
    assert_eq!(e.filename, "a.pdf");
    assert_eq!(e.mime_type, "application/pdf");
    assert_eq!(e.state, AttachmentState::Inline);

    let err = error(&c, fx.ids.not_downloaded, Some("slides.pdf"), None);
    assert!(matches!(err, ReadError::NotDownloaded { .. }), "{err:?}");
    assert!(
        err.to_string()
            .contains("never fetched from the server; open it in Mail once"),
        "{err}"
    );
    assert!(err.to_string().contains("slides.pdf"), "{err}");
}

#[test]
fn read_attachment_text_by_index_and_external() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);
    let e = text(&c, fx.ids.with_xlsx, None, Some(0));
    assert_eq!(e.filename, "a.xlsx");
    assert!(e.text.contains("xlsx sentence"), "text: {:?}", e.text);

    // A filename that matches nothing falls back to the index.
    let e = text(&c, fx.ids.with_docx, Some("other.docx"), Some(0));
    assert!(e.text.contains("docx sentence"), "text: {:?}", e.text);

    let e = text(&c, fx.ids.partial_external, Some("report.txt"), None);
    assert_eq!(e.text, "external report bytes\n");
    assert_eq!(e.state, AttachmentState::External);
}

#[test]
fn read_attachment_text_unselectable_or_unavailable() {
    let fx = fixture::build(Epoch::Unix);
    let c = ctx(&fx);

    let err = error(&c, fx.ids.with_pdf, Some("nope.pdf"), None);
    assert!(
        matches!(err, ReadError::Mime(MimeError::NoSuchPart(_))),
        "{err:?}"
    );
    assert!(err.to_string().contains("nope.pdf"), "{err}");

    let err = error(&c, fx.ids.with_pdf, None, Some(7));
    assert!(matches!(err, ReadError::Mime(MimeError::NoSuchPart(_))));
    assert!(err.to_string().contains('7'), "{err}");

    let err = error(&c, fx.ids.with_pdf, None, None);
    assert!(matches!(err, ReadError::Mime(MimeError::NoSuchPart(_))));

    let err = error(&c, fx.ids.not_extractable, Some("ghost.bin"), None);
    assert!(matches!(err, ReadError::NotExtractable { .. }), "{err:?}");
    assert!(
        err.to_string().contains("declared but not present on disk"),
        "{err}"
    );

    assert!(matches!(
        error(&c, 999_999, Some("a.pdf"), None),
        ReadError::NotFound(999_999)
    ));
}

#[test]
fn read_attachment_text_encrypted_failure_is_a_decryption_error() {
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
    let mut cfg = config(&fx);
    cfg.smime.decryptor = DecryptorKind::Openssl;
    cfg.smime.key = Some(dir.path().join("other.key"));
    cfg.smime.cert = Some(dir.path().join("other.crt"));
    let c = read::open(cfg).unwrap();
    let err = error(&c, id, None, Some(0));
    assert!(matches!(err, ReadError::Decrypt(_)), "{err:?}");
}
