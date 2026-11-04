mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use importin::config::{DecryptorKind, SmimeConfig};
use importin::emlx;
use importin::envelope;
use importin::mime;
use importin::paths::Locator;
use importin::smime::{
    self, DecryptError, Decryptor, EncryptionInfo, OpensslCli, SecurityCms, Unwrapped,
};
use importin::store;
use support::fixture::{self, Epoch, Fixture, SmimeKeys};

const TIMEOUT: Duration = Duration::from_secs(10);

/// The raw RFC 822 bytes of fixture message `id`.
fn load(fx: &Fixture, id: i64) -> Vec<u8> {
    let store = store::locate(&fx.mail_root).unwrap();
    let url = envelope::open(&store, &[], &Default::default())
        .unwrap()
        .get(id)
        .unwrap()
        .unwrap()
        .mailbox_url;
    let located = Locator::new(store).locate(id, &url).unwrap().unwrap();
    emlx::read(&located.emlx).unwrap().raw
}

fn openssl_with(keys: &SmimeKeys) -> OpensslCli {
    OpensslCli {
        bin: "openssl".into(),
        key: keys.key.clone(),
        cert: keys.cert.clone(),
        timeout: TIMEOUT,
    }
}

/// Run `openssl` in `dir`; no argument may contain a space.
fn openssl(dir: &Path, cmd: &str) {
    let out = Command::new("openssl")
        .args(cmd.split_whitespace())
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "openssl {cmd}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An executable `/bin/sh` script in `dir` that stands in for the openssl binary.
fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-openssl");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn fake_openssl(bin: PathBuf, timeout: Duration) -> OpensslCli {
    OpensslCli {
        bin,
        key: "unused-key.pem".into(),
        cert: "unused-cert.pem".into(),
        timeout,
    }
}

/// A decryptor that is never run: `decrypt` returns `plain`, `decode_signed` panics.
struct Stub {
    name: &'static str,
    plain: Option<&'static [u8]>,
}

impl Decryptor for Stub {
    fn decrypt(&self, _der: &[u8]) -> Result<Vec<u8>, DecryptError> {
        Ok(self.plain.expect("decrypt must not be called").to_vec())
    }
    fn decode_signed(&self, _der: &[u8]) -> Result<Vec<u8>, DecryptError> {
        panic!("decode_signed must not be called")
    }
    fn name(&self) -> &'static str {
        self.name
    }
}

fn body_text(u: &Unwrapped) -> String {
    mime::parse(&u.inner).unwrap().body(usize::MAX, false).text
}

#[test]
fn openssl_decrypts_fixture_to_signed_inner() {
    let fx = fixture::build(Epoch::Unix);
    let (Some(keys), Some(id)) = (&fx.smime, fx.ids.encrypted) else {
        eprintln!("skipped: the fixture has no S/MIME message (no openssl)");
        return;
    };
    let raw = load(&fx, id);
    let parsed = mime::parse(&raw).unwrap();
    let u = smime::unwrap(&parsed, &openssl_with(keys)).unwrap();
    assert!(u.signed);
    assert_eq!(u.layers, ["enveloped", "multipart/signed"]);
    let inner = mime::parse(&u.inner).unwrap();
    assert_eq!(inner.headers().content_type, "multipart/mixed");
    assert!(body_text(&u).contains("the sealed sentence"));
}

#[test]
fn wrong_key_is_failed_not_panic() {
    let fx = fixture::build(Epoch::Unix);
    let Some(id) = fx.ids.encrypted else {
        eprintln!("skipped: the fixture has no S/MIME message (no openssl)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    openssl(
        dir.path(),
        "req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=other -keyout other.key -out other.crt",
    );
    let d = OpensslCli {
        bin: "openssl".into(),
        key: dir.path().join("other.key"),
        cert: dir.path().join("other.crt"),
        timeout: TIMEOUT,
    };
    let raw = load(&fx, id);
    let parsed = mime::parse(&raw).unwrap();
    match smime::unwrap(&parsed, &d) {
        Err(DecryptError::Failed { status, stderr }) => {
            assert_ne!(status, 0);
            assert!(!stderr.is_empty());
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn not_enveloped_is_error() {
    let fx = fixture::build(Epoch::Unix);
    let never = Stub {
        name: "openssl",
        plain: None,
    };
    for id in [fx.ids.plain, fx.ids.signed_only] {
        let raw = load(&fx, id);
        let parsed = mime::parse(&raw).unwrap();
        assert!(matches!(
            smime::unwrap(&parsed, &never),
            Err(DecryptError::NotEnveloped)
        ));
    }
}

#[test]
fn opaque_signed_inner_is_decoded() {
    let fx = fixture::build(Epoch::Unix);
    let Some(keys) = &fx.smime else {
        eprintln!("skipped: the fixture has no S/MIME keys (no openssl)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("inner.eml"),
        "Content-Type: text/plain; charset=utf-8\n\nthe opaque sentence\n",
    )
    .unwrap();
    let (key, cert) = (keys.key.display(), keys.cert.display());
    openssl(
        dir.path(),
        &format!("cms -sign -nodetach -signer {cert} -inkey {key} -in inner.eml -out opaque.eml"),
    );
    openssl(
        dir.path(),
        &format!("cms -encrypt -aes256 -recip {cert} -in opaque.eml -out enc.eml"),
    );
    let raw = fs::read(dir.path().join("enc.eml")).unwrap();
    let parsed = mime::parse(&raw).unwrap();
    let u = smime::unwrap(&parsed, &openssl_with(keys)).unwrap();
    assert!(u.signed);
    assert_eq!(u.layers, ["enveloped", "signed-data"]);
    assert_eq!(body_text(&u).trim_end(), "the opaque sentence");
}

#[test]
fn decode_signed_ignores_a_broken_signature() {
    let fx = fixture::build(Epoch::Unix);
    let Some(keys) = &fx.smime else {
        eprintln!("skipped: the fixture has no S/MIME keys (no openssl)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("inner.eml"),
        "Content-Type: text/plain\n\nthe signed sentence\n",
    )
    .unwrap();
    let (key, cert) = (keys.key.display(), keys.cert.display());
    openssl(
        dir.path(),
        &format!(
            "cms -sign -nodetach -outform DER -signer {cert} -inkey {key} -in inner.eml -out signed.der"
        ),
    );
    let mut der = fs::read(dir.path().join("signed.der")).unwrap();
    let at = der
        .windows(19)
        .position(|w| w == b"the signed sentence")
        .expect("content octets in the DER");
    der[at] = b'T'; // the message digest no longer matches
    let out = openssl_with(keys).decode_signed(&der).unwrap();
    assert!(
        String::from_utf8_lossy(&out).contains("The signed sentence"),
        "{out:?}"
    );
}

#[test]
fn unsigned_inner_passes_through_and_auth_enveloped_is_tried() {
    let raw = b"From: a@example.test\nContent-Type: application/pkcs7-mime; smime-type=authEnveloped-data; name=\"smime.p7m\"\n\
Content-Transfer-Encoding: base64\n\nMAMCAQA=\n";
    let parsed = mime::parse(raw).unwrap();
    let d = Stub {
        name: "openssl",
        plain: Some(b"Content-Type: text/plain\n\nunsigned inside\n"),
    };
    let u = smime::unwrap(&parsed, &d).unwrap();
    assert!(!u.signed);
    assert_eq!(u.layers, ["enveloped"]);
    assert_eq!(u.inner, b"Content-Type: text/plain\n\nunsigned inside\n");
    // Debug never shows the decrypted bytes.
    assert_eq!(
        format!("{u:?}"),
        r#"Unwrapped { inner_len: 42, signed: false, layers: ["enveloped"] }"#
    );
}

#[test]
fn auth_enveloped_with_security_is_not_tried() {
    let raw = b"From: a@example.test\nContent-Type: application/pkcs7-mime; smime-type=authEnveloped-data; name=\"smime.p7m\"\n\
Content-Transfer-Encoding: base64\n\nMAMCAQA=\n";
    let parsed = mime::parse(raw).unwrap();
    let never = Stub {
        name: "security",
        plain: None,
    };
    let err = smime::unwrap(&parsed, &never).unwrap_err();
    assert!(matches!(err, DecryptError::AuthEnvelopedUnsupported));
    assert_eq!(
        err.to_string(),
        "authEnveloped-data not supported by security cms"
    );
}

#[test]
fn timeout_kills_child() {
    let dir = tempfile::tempdir().unwrap();
    let d = fake_openssl(script(dir.path(), "sleep 5"), Duration::from_millis(200));
    let start = Instant::now();
    let res = d.decrypt(&[0x30, 0x00]);
    let took = start.elapsed();
    assert!(matches!(res, Err(DecryptError::Timeout)), "{res:?}");
    assert!(took < Duration::from_secs(1), "took {took:?}");
}

#[test]
fn failed_keeps_the_last_600_bytes_of_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let bin = script(
        dir.path(),
        "printf 'first line\\n' >&2; i=0; while [ $i -lt 100 ]; do printf 'xxxxxxxxxx' >&2; i=$((i+1)); done; printf 'END\\n' >&2; exit 3",
    );
    match fake_openssl(bin, TIMEOUT).decrypt(&[0x30, 0x00]) {
        Err(DecryptError::Failed { status, stderr }) => {
            assert_eq!(status, 3);
            assert!(stderr.len() <= 600, "{} bytes", stderr.len());
            assert!(stderr.ends_with("xxEND"), "{stderr:?}");
            assert!(!stderr.contains("first line"));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn large_input_and_output_do_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let d = fake_openssl(script(dir.path(), "exec cat"), TIMEOUT);
    let input: Vec<u8> = (0..4 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(d.decrypt(&input).unwrap(), input);
    assert_eq!(d.decode_signed(&input).unwrap(), input);
}

#[test]
fn security_cms_spawns() {
    // Garbage DER fails in the decoder before any keychain is consulted.
    let d = SecurityCms { timeout: TIMEOUT };
    assert_eq!(d.name(), "security");
    match d.decrypt(&[0x30, 0x00]) {
        Err(DecryptError::Failed { status, stderr }) => {
            assert_ne!(status, 0);
            assert!(!stderr.is_empty());
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn security_cms_command_is_shared() {
    let args = |probe_only| {
        let cmd = SecurityCms::command(probe_only);
        assert_eq!(cmd.get_program(), "/usr/bin/security");
        cmd.get_args()
            .map(|a| a.to_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(args(false), ["cms", "-D"]);
    assert_eq!(args(true), ["cms", "-D", "-n"]);
}

#[test]
fn from_config_picks_the_decryptor() {
    let mut cfg = SmimeConfig {
        decryptor: DecryptorKind::Security,
        key: None,
        cert: None,
        openssl_bin: "openssl".into(),
    };
    assert_eq!(smime::from_config(&cfg).name(), "security");
    cfg.decryptor = DecryptorKind::Openssl;
    cfg.key = Some("k.pem".into());
    cfg.cert = Some("c.pem".into());
    assert_eq!(smime::from_config(&cfg).name(), "openssl");
}

#[test]
fn encryption_info_constructors() {
    let none = EncryptionInfo::none();
    assert!(!none.encrypted && !none.decrypted && !none.signed);
    assert_eq!(none.signature, "none");
    assert_eq!((none.method, none.error), (None, None));

    let failed = EncryptionInfo::failed("security", &DecryptError::AuthEnvelopedUnsupported);
    assert!(failed.encrypted && !failed.decrypted && !failed.signed);
    assert_eq!(failed.method.as_deref(), Some("security"));
    assert_eq!(failed.signature, "none");
    assert_eq!(
        failed.error.as_deref(),
        Some("authEnveloped-data not supported by security cms")
    );
    let json = serde_json::to_value(&failed).unwrap();
    assert_eq!(json["method"], "security");
    assert_eq!(json["decrypted"], false);
}
