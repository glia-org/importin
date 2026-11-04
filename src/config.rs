use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config parse error: {0}")]
    Parse(toml::de::Error),
    #[error("unknown config key: {0}")]
    UnknownKey(String),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecryptorKind {
    Security,
    Openssl,
}

#[derive(Debug, Clone)]
pub struct SmimeConfig {
    pub decryptor: DecryptorKind,
    pub key: Option<PathBuf>,
    pub cert: Option<PathBuf>,
    pub openssl_bin: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mail_root: PathBuf,
    pub exclude_accounts: Vec<String>,
    pub body_scan_days: u32,
    pub body_scan_max: usize,
    pub smime: SmimeConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawConfig {
    mail_root: String,
    exclude_accounts: Vec<String>,
    body_scan_days: u32,
    body_scan_max: usize,
    smime: RawSmime,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawSmime {
    decryptor: DecryptorKind,
    key: Option<String>,
    cert: Option<String>,
    openssl_bin: String,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            mail_root: "~/Library/Mail".into(),
            exclude_accounts: Vec::new(),
            body_scan_days: 90,
            body_scan_max: 500,
            smime: RawSmime::default(),
        }
    }
}

impl Default for RawSmime {
    fn default() -> Self {
        Self {
            decryptor: DecryptorKind::Security,
            key: None,
            cert: None,
            openssl_bin: "openssl".into(),
        }
    }
}

fn expand(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(p));
    }
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(p),
    }
}

/// Load from `$IMPORTIN_CONFIG` or `~/.config/importin/config.toml`; a missing file means defaults.
pub fn load() -> Result<Config, ConfigError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let path = match env.get("IMPORTIN_CONFIG") {
        Some(p) => expand(p),
        None => dirs::home_dir()
            .unwrap_or_default()
            .join(".config/importin/config.toml"),
    };
    read_file(&path, &env)
}

fn read_file(path: &Path, env: &HashMap<String, String>) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text, env),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => parse("", env),
        Err(e) => Err(e.into()),
    }
}

pub fn parse(toml_text: &str, env: &HashMap<String, String>) -> Result<Config, ConfigError> {
    let raw: RawConfig = toml::from_str(toml_text).map_err(|e| {
        let msg = e.message();
        match msg.strip_prefix("unknown field `") {
            Some(rest) => ConfigError::UnknownKey(rest.split('`').next().unwrap_or(rest).to_string()),
            None => ConfigError::Parse(e),
        }
    })?;
    let mail_root = match env.get("IMPORTIN_MAIL_ROOT") {
        Some(v) => expand(v),
        None => expand(&raw.mail_root),
    };
    let smime = SmimeConfig {
        decryptor: raw.smime.decryptor,
        key: raw.smime.key.as_deref().map(expand),
        cert: raw.smime.cert.as_deref().map(expand),
        openssl_bin: expand(&raw.smime.openssl_bin),
    };
    if smime.decryptor == DecryptorKind::Openssl && (smime.key.is_none() || smime.cert.is_none()) {
        return Err(ConfigError::Invalid(
            "smime.decryptor = \"openssl\" requires smime.key and smime.cert".into(),
        ));
    }
    Ok(Config {
        mail_root,
        exclude_accounts: raw.exclude_accounts,
        body_scan_days: raw.body_scan_days,
        body_scan_max: raw.body_scan_max,
        smime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn no_env() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn defaults_when_empty() {
        let c = parse("", &no_env()).unwrap();
        assert!(c.mail_root.ends_with("Library/Mail"));
        assert!(c.exclude_accounts.is_empty());
        assert_eq!(c.body_scan_days, 90);
        assert_eq!(c.body_scan_max, 500);
        assert_eq!(c.smime.decryptor, DecryptorKind::Security);
        assert_eq!(c.smime.openssl_bin, PathBuf::from("openssl"));
    }

    #[test]
    fn env_overrides_mail_root() {
        let env = HashMap::from([("IMPORTIN_MAIL_ROOT".to_string(), "/tmp/x".to_string())]);
        let c = parse("mail_root = \"/other\"", &env).unwrap();
        assert_eq!(c.mail_root, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn unknown_key_is_error() {
        assert!(matches!(parse("foo = 1", &no_env()), Err(ConfigError::UnknownKey(_))));
    }

    #[test]
    fn smime_openssl_needs_key_and_cert() {
        let r = parse("[smime]
decryptor = \"openssl\"", &no_env());
        assert!(matches!(r, Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn tilde_expanded() {
        let c = parse("mail_root = \"~/M\"", &no_env()).unwrap();
        assert!(c.mail_root.starts_with(dirs::home_dir().unwrap()));
        assert!(c.mail_root.ends_with("M"));
    }
}
