//! Text from attachment bytes: PDF, Word, spreadsheets, PowerPoint, HTML and plain text. A
//! document that cannot be read gives empty text and a note, never an error or a panic.

use std::io::{Cursor, Read};
use std::panic::{self, UnwindSafe};
use std::path::Path;

use calamine::Reader as _;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Serialize;
use zip::ZipArchive;

use crate::emlx::MAX_EMLX_BYTES;
use crate::mime::{self, AttachmentState};

/// The note for a zip that inflates, or declares it inflates, past `MAX_EMLX_BYTES`.
const TOO_LARGE: &str = "attachment inflates past the 64 MiB limit";

/// No `Debug`: the text may come from a decrypted message.
#[derive(Serialize)]
pub struct Extracted {
    pub filename: String,
    pub mime_type: String,
    pub state: AttachmentState,
    pub text: String,
    pub truncated: bool,
    pub note: Option<String>,
}

enum Kind {
    Pdf,
    Docx,
    Sheet,
    Pptx,
    Html,
    Text,
    Other,
}

/// By lowercase extension first, then by MIME type. `state` is `Inline` (the bytes are at hand);
/// `read::attachment_text` sets the selected attachment's state.
pub fn extract(bytes: &[u8], filename: &str, mime_type: &str, max_chars: usize) -> Extracted {
    let (text, note) = match kind(filename, mime_type) {
        Kind::Pdf => guarded("pdf", || {
            pdf_extract::extract_text_from_mem(bytes).map_err(|e| e.to_string())
        }),
        Kind::Docx => guarded("docx", || docx(bytes)),
        Kind::Sheet => guarded("spreadsheet", || sheets(bytes)),
        Kind::Pptx => guarded("pptx", || pptx(bytes)),
        Kind::Html => (mime::strip_html(&String::from_utf8_lossy(bytes)), None),
        Kind::Text => (String::from_utf8_lossy(bytes).into_owned(), None),
        Kind::Other => (
            String::new(),
            Some(format!(
                "{mime_type}, {} bytes, no text extraction",
                bytes.len()
            )),
        ),
    };
    let (text, truncated) = mime::truncate(text, max_chars);
    Extracted {
        filename: filename.to_string(),
        mime_type: mime_type.to_string(),
        state: AttachmentState::Inline,
        text,
        truncated,
        note,
    }
}

fn kind(filename: &str, mime_type: &str) -> Kind {
    let ext = Path::new(filename)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("pdf") => return Kind::Pdf,
        Some("docx") => return Kind::Docx,
        Some("xlsx" | "xls" | "xlsb" | "ods") => return Kind::Sheet,
        Some("pptx") => return Kind::Pptx,
        Some("html" | "htm") => return Kind::Html,
        Some("txt" | "md" | "csv") => return Kind::Text,
        _ => {}
    }
    match mime_type.to_ascii_lowercase().as_str() {
        "application/pdf" => Kind::Pdf,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Kind::Docx,
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Kind::Pptx,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.ms-excel"
        | "application/vnd.ms-excel.sheet.binary.macroenabled.12"
        | "application/vnd.oasis.opendocument.spreadsheet" => Kind::Sheet,
        "text/html" => Kind::Html,
        m if m.starts_with("text/") => Kind::Text,
        _ => Kind::Other,
    }
}

/// An error or a panic in a parser becomes empty text and the note "<kind> extraction failed: …";
/// [`TOO_LARGE`] is the note as it stands.
fn guarded(
    kind: &str,
    f: impl FnOnce() -> Result<String, String> + UnwindSafe,
) -> (String, Option<String>) {
    let why = match panic::catch_unwind(f) {
        Ok(Ok(text)) => return (text, None),
        Ok(Err(why)) if why == TOO_LARGE => return (String::new(), Some(why)),
        Ok(Err(why)) => why,
        Err(payload) => payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string()),
    };
    (
        String::new(),
        Some(format!("{kind} extraction failed: {why}")),
    )
}

/// `<w:t>` text of `word/document.xml`, a newline after each paragraph.
fn docx(bytes: &[u8]) -> Result<String, String> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let xml = entry(&mut zip, "word/document.xml")?;
    xml_text(&xml, "w:t", "w:p")
}

/// `== <sheet>`, then one line per row, cells tab-separated.
fn sheets(bytes: &[u8]) -> Result<String, String> {
    // calamine inflates the parts itself; zip refuses to inflate an entry past its declared size,
    // so the declared sizes of a zip workbook (xlsx, xlsb, ods) bound what calamine reads. xls is
    // not a zip.
    if let Ok(zip) = ZipArchive::new(Cursor::new(bytes)) {
        let mut declared: u64 = 0;
        for i in 0..zip.len() {
            let size = zip.by_index_data(i).map_err(|e| e.to_string())?.size();
            declared = declared.saturating_add(size);
        }
        if declared > MAX_EMLX_BYTES {
            return Err(TOO_LARGE.into());
        }
    }
    let mut book =
        calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut out = String::new();
    for name in book.sheet_names() {
        let range = book.worksheet_range(&name).map_err(|e| e.to_string())?;
        out += &format!("== {name}\n");
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(ToString::to_string).collect();
            out += &cells.join("\t");
            out.push('\n');
        }
    }
    Ok(out)
}

/// `== slide N`, then the `<a:t>` text of `ppt/slides/slideN.xml`, a newline after each paragraph.
fn pptx(bytes: &[u8]) -> Result<String, String> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut slides: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(Result::ok)
        .filter_map(|name| {
            let n = name
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse()
                .ok()?;
            Some((n, name.into_owned()))
        })
        .collect();
    slides.sort();
    let mut out = String::new();
    for (n, name) in slides {
        out += &format!("== slide {n}\n");
        out += &xml_text(&entry(&mut zip, &name)?, "a:t", "a:p")?;
    }
    Ok(out)
}

/// The entry as text, inflated to at most `MAX_EMLX_BYTES`.
fn entry(zip: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<String, String> {
    let mut xml = Vec::new();
    zip.by_name(name)
        .map_err(|e| format!("{name}: {e}"))?
        .take(MAX_EMLX_BYTES + 1)
        .read_to_end(&mut xml)
        .map_err(|e| format!("{name}: {e}"))?;
    if xml.len() as u64 > MAX_EMLX_BYTES {
        return Err(TOO_LARGE.into());
    }
    String::from_utf8(xml).map_err(|e| format!("{name}: {e}"))
}

/// The text inside `text` elements, a newline at each `</para>`.
fn xml_text(xml: &str, text: &str, para: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    let (mut out, mut inside) = (String::new(), false);
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) if e.name().as_ref() == text => inside = true,
            Event::End(e) if e.name().as_ref() == text => inside = false,
            Event::End(e) if e.name().as_ref() == para => out.push('\n'),
            Event::Text(t) if inside => out += &t.xml10_content(),
            Event::CData(t) if inside => out += &t.xml10_content(),
            Event::GeneralRef(r) if inside => match r.resolve_char_ref() {
                Ok(Some(c)) => out.push(c),
                _ => {
                    out += match &*r {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    }
                }
            },
            Event::Eof => return Ok(out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::{guarded, pptx, xml_text};

    #[test]
    fn a_parser_panic_becomes_a_note() {
        let (text, note) = guarded("pdf", || panic!("bad xref"));
        assert_eq!(text, "");
        assert_eq!(note.as_deref(), Some("pdf extraction failed: bad xref"));
    }

    #[test]
    fn xml_text_resolves_references_and_skips_other_elements() {
        let xml = "<w:p><w:r><w:t>a &amp; b &#233;</w:t></w:r><w:x>no</w:x></w:p><w:t>c</w:t>";
        assert_eq!(xml_text(xml, "w:t", "w:p").unwrap(), "a & b \u{e9}\nc");
    }

    #[test]
    fn pptx_slides_in_numeric_order() {
        let slide = |t: &str| format!("<p:sld><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>");
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in [
            ("ppt/slides/slide10.xml", slide("ten")),
            ("ppt/slides/slide2.xml", slide("two")),
            (
                "ppt/slides/_rels/slide2.xml.rels",
                "<Relationships/>".to_string(),
            ),
        ] {
            w.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        let bytes = w.finish().unwrap().into_inner();
        assert_eq!(pptx(&bytes).unwrap(), "== slide 2\ntwo\n== slide 10\nten\n");
    }
}
