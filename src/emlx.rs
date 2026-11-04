//! Split an `.emlx` file: `<decimal count><whitespace><count bytes of RFC 822><XML plist trailer>`.

use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const MAX_EMLX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Emlx {
    pub raw: Vec<u8>,
    pub flags: Flags,
    /// The trailer's `date-received`, Unix seconds.
    pub date_received: Option<i64>,
}

/// The trailer's `flags` integer. Hints only: the content type decides encryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(pub u64);

impl Flags {
    fn bit(self, n: u32) -> bool {
        (self.0 >> n) & 1 == 1
    }
    pub fn read(self) -> bool {
        self.bit(0)
    }
    pub fn deleted(self) -> bool {
        self.bit(1)
    }
    pub fn answered(self) -> bool {
        self.bit(2)
    }
    pub fn encrypted(self) -> bool {
        self.bit(3)
    }
    pub fn flagged(self) -> bool {
        self.bit(4)
    }
    pub fn draft(self) -> bool {
        self.bit(6)
    }
    pub fn attachment_count(self) -> u32 {
        ((self.0 >> 10) & 0x3f) as u32
    }
    pub fn signed(self) -> bool {
        self.bit(23)
    }
    pub fn junk(self) -> bool {
        self.bit(24)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmlxError {
    #[error("emlx is {bytes} bytes, over the {max}-byte limit")]
    TooLarge { bytes: u64, max: u64 },
    #[error("emlx does not start with a byte count")]
    BadPrefix,
    #[error("emlx is shorter than its byte count")]
    Truncated,
    #[error("emlx: {0}")]
    Io(#[from] std::io::Error),
}

pub fn parse(bytes: &[u8]) -> Result<Emlx, EmlxError> {
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    let count: usize = std::str::from_utf8(&bytes[..digits])
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(EmlxError::BadPrefix)?;
    // Any whitespace ends the count, up to and including the first newline.
    let mut start = digits;
    while let Some(b) = bytes.get(start).filter(|b| b.is_ascii_whitespace()) {
        start += 1;
        if *b == b'\n' {
            break;
        }
    }
    if start == digits {
        return Err(EmlxError::BadPrefix);
    }
    let end = start.checked_add(count).ok_or(EmlxError::Truncated)?;
    let (mut raw, trailer) = match (bytes.get(start..end), bytes.get(end..)) {
        (Some(r), Some(t)) => (r.to_vec(), t),
        _ => return Err(EmlxError::Truncated),
    };
    if lost_closing_dash(&raw, trailer) {
        raw.push(b'-');
    }
    let dict = plist::Value::from_reader_xml(trailer).ok();
    let dict = dict.as_ref().and_then(plist::Value::as_dictionary);
    let flags = dict
        .and_then(|d| d.get("flags"))
        .and_then(plist::Value::as_unsigned_integer)
        .unwrap_or(0);
    let date_received = dict
        .and_then(|d| d.get("date-received"))
        .and_then(plist::Value::as_signed_integer);
    Ok(Emlx {
        raw,
        flags: Flags(flags),
        date_received,
    })
}

/// True if the payload's last line is a closing boundary one dash short (`--token-`) and a plist
/// follows directly. The dash is missing from the file itself (the count matches the bytes on disk);
/// restoring it lets the MIME parser see the closing boundary.
fn lost_closing_dash(raw: &[u8], trailer: &[u8]) -> bool {
    let line = raw.rsplit(|&b| b == b'\n').next().unwrap_or(raw);
    let plist_follows = trailer.starts_with(b"<?xml") || trailer.starts_with(b"<plist");
    plist_follows
        && line.len() > 3
        && line.starts_with(b"--")
        && line.ends_with(b"-")
        && !line.ends_with(b"--")
}

/// Reads and parses an emlx; files over `MAX_EMLX_BYTES` are refused before any byte is read.
pub fn read(path: &Path) -> Result<Emlx, EmlxError> {
    let mut f = File::open(path)?;
    let bytes = f.metadata()?.len();
    if bytes > MAX_EMLX_BYTES {
        return Err(EmlxError::TooLarge {
            bytes,
            max: MAX_EMLX_BYTES,
        });
    }
    let mut buf = Vec::with_capacity(bytes as usize);
    f.read_to_end(&mut buf)?;
    parse(&buf)
}
