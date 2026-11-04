//! Map a message ROWID and mailbox URL to its `.emlx` file and `Attachments/` directory: compute the
//! path first, walk the mailbox's `Data` tree second (DESIGN §5).

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use crate::envelope::parse_mailbox_url;
use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Formula,
    Walk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub emlx: PathBuf,
    /// `…/Data/<shard>/Attachments/<ROWID>`, the sibling of the emlx's `Messages` dir.
    pub attachments_dir: PathBuf,
    pub by: Resolution,
}

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// What a walk result is valid for: a change to the Envelope Index or its WAL invalidates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    mtime: Option<SystemTime>,
    size: Option<u64>,
    wal_mtime: Option<SystemTime>,
}

struct MailboxCache {
    stamp: Stamp,
    files: HashMap<i64, PathBuf>,
}

pub struct Locator {
    store: Store,
    /// Per mailbox URL, the ROWID → emlx map from its last walk.
    cache: Mutex<HashMap<String, MailboxCache>>,
}

/// The digits of `rowid / 1000` reversed, one directory each: 1_234_567 → `4/3/2/1`; below 1000 none.
pub fn shard(rowid: i64) -> PathBuf {
    match rowid / 1000 {
        n if n <= 0 => PathBuf::new(),
        n => n.to_string().chars().rev().map(String::from).collect(),
    }
}

/// The mailbox directories a URL may name. An account uuid names one; any other authority (user@host)
/// yields every account dir under the version dir that holds the `<Seg>.mbox` chain.
pub fn mailbox_dir_candidates(store: &Store, url: &str) -> Vec<PathBuf> {
    let parts = parse_mailbox_url(url);
    let chain: PathBuf = parts
        .mailbox_name
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| format!("{s}.mbox"))
        .collect();
    if chain.as_os_str().is_empty() {
        return Vec::new();
    }
    if let Some(uuid) = parts.account_uuid {
        return vec![store.version_dir.join(uuid).join(chain)];
    }
    let Ok(entries) = std::fs::read_dir(&store.version_dir) else {
        return Vec::new();
    };
    let mut accounts: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_name() != "MailData")
        .map(|e| e.path())
        .collect();
    accounts.sort();
    accounts
        .into_iter()
        .map(|a| a.join(&chain))
        .filter(|d| d.is_dir())
        .collect()
}

/// The first (by name) 36-character child of a mailbox dir that contains `Data`.
pub fn store_uuid_dir(mailbox_dir: &Path) -> Option<PathBuf> {
    store_dirs(mailbox_dir).into_iter().next()
}

fn store_dirs(mailbox_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(mailbox_dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_name().len() == 36)
        .map(|e| e.path())
        .filter(|p| p.join("Data").is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn located(emlx: PathBuf, rowid: i64, by: Resolution) -> Located {
    // `emlx` is always `<shard dir>/Messages/<file>`.
    let shard_dir = emlx
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new(""));
    let attachments_dir = shard_dir.join("Attachments").join(rowid.to_string());
    Located {
        emlx,
        attachments_dir,
        by,
    }
}

fn stamp(store: &Store) -> Stamp {
    let meta = std::fs::metadata(&store.envelope_index).ok();
    let mut wal = store.envelope_index.clone().into_os_string();
    wal.push("-wal");
    Stamp {
        mtime: meta.as_ref().and_then(|m| m.modified().ok()),
        size: meta.as_ref().map(|m| m.len()),
        wal_mtime: std::fs::metadata(wal).and_then(|m| m.modified()).ok(),
    }
}

/// `<ROWID>.emlx` or `<ROWID>.partial.emlx` → ROWID.
fn rowid_of(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".emlx")?;
    let stem = stem.strip_suffix(".partial").unwrap_or(stem);
    stem.parse().ok()
}

/// Every emlx under the `Data` trees of the given mailbox dirs, in order; the first path per ROWID wins.
fn walk(mailbox_dirs: &[PathBuf]) -> Result<HashMap<i64, PathBuf>, PathError> {
    let mut files = HashMap::new();
    for data in mailbox_dirs.iter().flat_map(|d| store_dirs(d)) {
        let mut stack = vec![data.join("Data")];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(source) => return Err(PathError::Io { path: dir, source }),
            };
            let in_messages = dir.file_name().is_some_and(|n| n == "Messages");
            for entry in entries {
                let entry = entry.map_err(|source| PathError::Io {
                    path: dir.clone(),
                    source,
                })?;
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name();
                if kind.is_dir() && name != "Attachments" {
                    stack.push(entry.path());
                } else if kind.is_file()
                    && in_messages
                    && let Some(rowid) = name.to_str().and_then(rowid_of)
                {
                    files.entry(rowid).or_insert_with(|| entry.path());
                }
            }
        }
    }
    Ok(files)
}

impl Locator {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cache(&self) -> MutexGuard<'_, HashMap<String, MailboxCache>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `Ok(None)` if neither the formula nor the (cached) walk of the mailbox finds the file. A cached
    /// path that no longer exists costs one fresh walk of the mailbox.
    pub fn locate(&self, rowid: i64, mailbox_url: &str) -> Result<Option<Located>, PathError> {
        let dirs = mailbox_dir_candidates(&self.store, mailbox_url);
        let messages = shard(rowid).join("Messages");
        for dir in &dirs {
            let Some(store_dir) = store_uuid_dir(dir) else {
                continue;
            };
            let base = store_dir.join("Data").join(&messages);
            for name in [format!("{rowid}.emlx"), format!("{rowid}.partial.emlx")] {
                let path = base.join(name);
                if path.is_file() {
                    return Ok(Some(located(path, rowid, Resolution::Formula)));
                }
            }
        }

        let now = stamp(&self.store);
        if let Some(c) = self.cache().get(mailbox_url)
            && c.stamp == now
        {
            match c.files.get(&rowid) {
                None => return Ok(None),
                Some(p) if p.is_file() => {
                    return Ok(Some(located(p.clone(), rowid, Resolution::Walk)));
                }
                // Moved since the walk: walk once more; the new map replaces this entry.
                Some(_) => {}
            }
        }
        let files = walk(&dirs)?;
        let hit = files.get(&rowid).cloned();
        self.cache()
            .insert(mailbox_url.to_owned(), MailboxCache { stamp: now, files });
        Ok(hit.map(|p| located(p, rowid, Resolution::Walk)))
    }
}
