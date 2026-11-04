//! Find the Mail store: `<mail_root>/V<n>` (highest n) and its Envelope Index; classify access errors.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct Store {
    pub mail_root: PathBuf,
    pub version_dir: PathBuf,
    pub envelope_index: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}: missing or access denied (Full Disk Access not granted?)", path.display())]
    MissingOrDenied { path: PathBuf },
    #[error("{}: no V<n> directory", path.display())]
    NoVersionDir { path: PathBuf },
    #[error("{}: access undetermined: {source}", path.display())]
    Undetermined {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn classify(path: &Path, e: std::io::Error) -> StoreError {
    match e.kind() {
        // EPERM and EACCES both map to PermissionDenied.
        ErrorKind::NotFound | ErrorKind::PermissionDenied => StoreError::MissingOrDenied {
            path: path.to_path_buf(),
        },
        _ => StoreError::Undetermined {
            path: path.to_path_buf(),
            source: e,
        },
    }
}

pub fn locate(mail_root: &Path) -> Result<Store, StoreError> {
    let entries = std::fs::read_dir(mail_root).map_err(|e| classify(mail_root, e))?;
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in entries {
        let entry = entry.map_err(|e| classify(mail_root, e))?;
        let name = entry.file_name();
        let Some(n) = name
            .to_str()
            .and_then(|s| s.strip_prefix('V'))
            .and_then(|d| d.parse::<u64>().ok())
        else {
            continue;
        };
        if entry.path().is_dir() && best.as_ref().is_none_or(|(m, _)| n > *m) {
            best = Some((n, entry.path()));
        }
    }
    let Some((_, version_dir)) = best else {
        return Err(StoreError::NoVersionDir {
            path: mail_root.to_path_buf(),
        });
    };
    std::fs::read_dir(&version_dir).map_err(|e| classify(&version_dir, e))?;
    Ok(Store {
        mail_root: mail_root.to_path_buf(),
        envelope_index: version_dir.join("MailData").join("Envelope Index"),
        version_dir,
    })
}

/// Open the Envelope Index and read its 16-byte SQLite header; returns the file size.
pub fn probe_access(store: &Store) -> Result<u64, StoreError> {
    let path = &store.envelope_index;
    let mut f = File::open(path).map_err(|e| classify(path, e))?;
    let mut header = [0u8; 16];
    f.read_exact(&mut header).map_err(|e| classify(path, e))?;
    if &header != b"SQLite format 3\0" {
        return Err(StoreError::Undetermined {
            path: path.clone(),
            source: std::io::Error::new(ErrorKind::InvalidData, "not an SQLite database"),
        });
    }
    let meta = f.metadata().map_err(|e| classify(path, e))?;
    Ok(meta.len())
}

/// True if a process named exactly `Mail` runs (`pgrep -x Mail`); false if pgrep is unavailable.
pub fn mail_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "Mail"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn envelope_mtime(store: &Store) -> Option<SystemTime> {
    std::fs::metadata(&store.envelope_index)
        .and_then(|m| m.modified())
        .ok()
}
