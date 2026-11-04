//! S/MIME: decrypt an enveloped message with an external decoder (`security cms` or `openssl cms`)
//! and unwrap one signed layer. Signatures are not verified. DER goes to the decoder on stdin and
//! the content comes back on stdout; nothing is written to disk.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{DecryptorKind, SmimeConfig};
use crate::mime;

const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const STDERR_TAIL: usize = 600;

#[derive(Debug, thiserror::Error)]
pub enum DecryptError {
    #[error("decryptor timed out")]
    Timeout,
    #[error("cannot run decryptor: {0}")]
    Spawn(io::Error),
    #[error("decryptor failed with exit status {status}: {stderr}")]
    Failed { status: i32, stderr: String },
    #[error("authEnveloped-data not supported by security cms")]
    AuthEnvelopedUnsupported,
    #[error("not an S/MIME enveloped message with a DER body")]
    NotEnveloped,
}

pub trait Decryptor: Send + Sync {
    /// Enveloped-data DER in, the decrypted MIME entity out.
    fn decrypt(&self, der: &[u8]) -> Result<Vec<u8>, DecryptError>;
    /// Signed-data DER in, the signed content out (signature not verified).
    fn decode_signed(&self, der: &[u8]) -> Result<Vec<u8>, DecryptError>;
    fn name(&self) -> &'static str;
}

/// `/usr/bin/security cms -D`, with the identities of the keychain search list.
pub struct SecurityCms {
    pub timeout: Duration,
}

impl SecurityCms {
    fn decode(&self, der: &[u8]) -> Result<Vec<u8>, DecryptError> {
        let mut cmd = Command::new("/usr/bin/security");
        cmd.args(["cms", "-D"]);
        run(cmd, der, self.timeout)
    }
}

impl Decryptor for SecurityCms {
    fn decrypt(&self, der: &[u8]) -> Result<Vec<u8>, DecryptError> {
        self.decode(der)
    }
    fn decode_signed(&self, der: &[u8]) -> Result<Vec<u8>, DecryptError> {
        self.decode(der)
    }
    fn name(&self) -> &'static str {
        "security"
    }
}

/// `openssl cms` with a PEM key and certificate.
pub struct OpensslCli {
    pub bin: PathBuf,
    pub key: PathBuf,
    pub cert: PathBuf,
    pub timeout: Duration,
}

impl Decryptor for OpensslCli {
    fn decrypt(&self, der: &[u8]) -> Result<Vec<u8>, DecryptError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(["cms", "-decrypt", "-inform", "DER", "-inkey"])
            .arg(&self.key)
            .arg("-recip")
            .arg(&self.cert);
        run(cmd, der, self.timeout)
    }
    fn decode_signed(&self, der: &[u8]) -> Result<Vec<u8>, DecryptError> {
        let mut cmd = Command::new(&self.bin);
        // -nosigs: content comes back even when the signature would not verify.
        cmd.args(["cms", "-verify", "-noverify", "-nosigs", "-inform", "DER"]);
        run(cmd, der, self.timeout)
    }
    fn name(&self) -> &'static str {
        "openssl"
    }
}

pub fn from_config(cfg: &SmimeConfig) -> Box<dyn Decryptor> {
    match cfg.decryptor {
        DecryptorKind::Security => Box::new(SecurityCms { timeout: TIMEOUT }),
        DecryptorKind::Openssl => Box::new(OpensslCli {
            bin: cfg.openssl_bin.clone(),
            key: cfg
                .key
                .clone()
                .expect("config validated: openssl needs smime.key"),
            cert: cfg
                .cert
                .clone()
                .expect("config validated: openssl needs smime.cert"),
            timeout: TIMEOUT,
        }),
    }
}

pub struct Unwrapped {
    /// A MIME entity to parse with `mime::parse`.
    pub inner: Vec<u8>,
    pub signed: bool,
    /// The layers removed, outermost first: `["enveloped", "multipart/signed"]`.
    pub layers: Vec<&'static str>,
}

/// Never shows the decrypted bytes, so `{:?}` cannot leak them into a log.
impl fmt::Debug for Unwrapped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Unwrapped")
            .field("inner_len", &self.inner.len())
            .field("signed", &self.signed)
            .field("layers", &self.layers)
            .finish()
    }
}

/// Decrypt an enveloped message and remove one signed layer (multipart/signed or opaque
/// signed-data) if the decrypted entity has one. Deeper nesting is left in `inner`.
pub fn unwrap(parsed: &mime::Parsed<'_>, d: &dyn Decryptor) -> Result<Unwrapped, DecryptError> {
    if !parsed.is_enveloped() {
        return Err(DecryptError::NotEnveloped);
    }
    if parsed.is_auth_enveloped() && d.name() == "security" {
        return Err(DecryptError::AuthEnvelopedUnsupported);
    }
    let der = parsed.enveloped_der().ok_or(DecryptError::NotEnveloped)?;
    let plain = d.decrypt(&der)?;
    if let Ok(p) = mime::parse(&plain) {
        if let Some(first) = p.signed_first_part() {
            return Ok(Unwrapped {
                inner: first.to_vec(),
                signed: true,
                layers: vec!["enveloped", "multipart/signed"],
            });
        }
        if let Some(signed) = p.signed_der() {
            return Ok(Unwrapped {
                inner: d.decode_signed(&signed)?,
                signed: true,
                layers: vec!["enveloped", "signed-data"],
            });
        }
    }
    Ok(Unwrapped {
        inner: plain,
        signed: false,
        layers: vec!["enveloped"],
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EncryptionInfo {
    pub encrypted: bool,
    pub decrypted: bool,
    /// The decryptor's name, "security" or "openssl".
    pub method: Option<String>,
    pub signed: bool,
    /// "not verified" or "none".
    pub signature: String,
    pub error: Option<String>,
}

impl EncryptionInfo {
    pub fn none() -> Self {
        Self {
            encrypted: false,
            decrypted: false,
            method: None,
            signed: false,
            signature: "none".into(),
            error: None,
        }
    }

    pub fn failed(method: &str, err: &DecryptError) -> Self {
        Self {
            encrypted: true,
            method: Some(method.into()),
            error: Some(err.to_string()),
            ..Self::none()
        }
    }
}

/// Run `cmd` with `input` on stdin; stdout on success. stdin is written and stdout/stderr are read
/// on their own threads, so a full pipe never blocks the child. On timeout the child is killed and
/// the pipe threads are left to finish when the pipes close.
fn run(mut cmd: Command, input: &[u8], timeout: Duration) -> Result<Vec<u8>, DecryptError> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(DecryptError::Spawn)?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input = input.to_vec();
    // A child that exits without reading gives EPIPE; its exit status tells what went wrong.
    thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let stdout = read_all(child.stdout.take().expect("stdout is piped"));
    let stderr = read_all(child.stderr.take().expect("stderr is piped"));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => thread::sleep(POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DecryptError::Timeout);
            }
            Err(e) => {
                let _ = child.kill();
                return Err(DecryptError::Spawn(e));
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    Err(DecryptError::Failed {
        // No code when killed by a signal.
        status: status.code().unwrap_or(-1),
        stderr: tail(&stderr, STDERR_TAIL),
    })
}

fn read_all<R: Read + Send + 'static>(mut pipe: R) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// The last `max` bytes of `bytes` as trimmed text, cut on a char boundary.
fn tail(bytes: &[u8], max: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}
