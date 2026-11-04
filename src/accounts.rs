//! Account labels from the macOS Accounts database (`~/Library/Accounts/Accounts4.sqlite`): the
//! username, name and type behind the account uuid of a `mailbox://<uuid>/…` URL. Opened read-only
//! the way the Envelope Index is; read once at startup. Nothing is ever written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::envelope;

/// The accounts with their type; `ZACCOUNTTYPE` is a foreign key into `ZACCOUNTTYPE.Z_PK`.
const QUERY: &str = "SELECT a.ZIDENTIFIER, a.ZUSERNAME, a.ZACCOUNTDESCRIPTION, t.ZACCOUNTTYPEDESCRIPTION \
     FROM ZACCOUNT a LEFT JOIN ZACCOUNTTYPE t ON t.Z_PK = a.ZACCOUNTTYPE \
     WHERE a.ZIDENTIFIER IS NOT NULL";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLabel {
    /// `ZIDENTIFIER`, as stored.
    pub uuid: String,
    /// `ZUSERNAME`, often an address.
    pub username: Option<String>,
    /// `ZACCOUNTDESCRIPTION`, the account's name as the user sees it.
    pub description: Option<String>,
    /// `ZACCOUNTTYPEDESCRIPTION`, e.g. "IMAP", "Google", "iCloud" or "On My Device".
    pub kind: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    #[error("cannot read the Accounts database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// `~/Library/Accounts/Accounts4.sqlite`.
pub fn default_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join("Library/Accounts/Accounts4.sqlite")
}

/// Every account, keyed by its uppercased uuid. Opened `file:<path>?mode=ro` with
/// `READ_ONLY | URI | NO_MUTEX` and a 2 s busy timeout, as the Envelope Index is.
pub fn load(path: &Path) -> Result<HashMap<String, AccountLabel>, AccountsError> {
    let conn = envelope::connect(path)?;
    let mut stmt = conn.prepare(QUERY)?;
    let rows = stmt.query_map([], |r| {
        Ok(AccountLabel {
            uuid: r.get(0)?,
            username: r.get(1)?,
            description: r.get(2)?,
            kind: r.get(3)?,
        })
    })?;
    let mut labels = HashMap::new();
    for label in rows {
        let label = label?;
        labels.insert(label.uuid.to_ascii_uppercase(), label);
    }
    Ok(labels)
}
