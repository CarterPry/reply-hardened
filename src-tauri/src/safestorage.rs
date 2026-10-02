//! Recovering the SQLCipher key for Signal Desktop's message database.
//!
//! Signal Desktop (like every Electron app that uses Chromium's
//! `os_crypt`) keeps its 32-byte SQLCipher key in `config.json`, itself
//! encrypted under a random password that lives in the login keychain
//! as the generic password **"Signal Safe Storage"**. The wrapping is
//! Chromium's, not Signal's, and has been stable for years:
//!
//!   password  = keychain item "Signal Safe Storage"
//!   kek       = PBKDF2-HMAC-SHA1(password, "saltysalt", 1003, 16)
//!   plaintext = AES-128-CBC-decrypt(kek, iv = 16 spaces, ciphertext)
//!
//! where `ciphertext` is `config.json`'s `encryptedKey` hex minus its
//! 3-byte `v10` version tag, and `plaintext` is the 64-character hex
//! SQLCipher key. Older installs store the key unwrapped in the `key`
//! field, which we accept as-is.
//!
//! Everything here is read-only and local: one keychain read and one
//! file read. The password, the derived KEK and the database key are
//! held in `Secret`, which zeroes its buffer on drop, and are never
//! logged, serialized, or passed on a command line.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::path::{Path, PathBuf};

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// Chromium's `os_crypt` constants. Not configurable - they are what
/// the data on disk was written with.
const SALT: &[u8] = b"saltysalt";
const ITERATIONS: u32 = 1003;
const KEK_LEN: usize = 16;
const IV: [u8; 16] = [b' '; 16];
const VERSION_TAG: &[u8] = b"v10";
/// The keychain account/service Signal Desktop writes.
const KEYCHAIN_SERVICE: &str = "Signal Safe Storage";

/// A byte buffer that is wiped when it goes out of scope. Not a
/// hardware-grade guarantee (the OS may still have paged it), just the
/// baseline hygiene of not leaving key material in freed heap.
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// The key as SQLCipher wants it: 64 lowercase hex characters.
    pub fn as_hex_str(&self) -> Option<&str> {
        let s = std::str::from_utf8(&self.0).ok()?;
        let s = s.trim();
        if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(s)
        } else {
            None
        }
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // write_volatile so the zeroing isn't optimized away
        for b in self.0.iter_mut() {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} bytes)", self.0.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// No Signal Desktop profile on this machine.
    ConfigMissing(String),
    /// config.json is unreadable (usually Full Disk Access).
    ConfigUnreadable(String),
    /// config.json parsed but holds neither `key` nor `encryptedKey`.
    ConfigHasNoKey,
    /// The keychain lookup failed or the user denied the prompt.
    KeychainDenied(String),
    /// The unwrap produced something that isn't a SQLCipher key.
    Undecryptable(String),
}

impl KeyError {
    /// One-line, content-free explanation for the UI.
    pub fn detail(&self) -> String {
        match self {
            KeyError::ConfigMissing(_) => "Signal Desktop isn't set up on this Mac".to_string(),
            KeyError::ConfigUnreadable(_) => {
                "Can't read Signal's config - grant Full Disk Access".to_string()
            }
            KeyError::ConfigHasNoKey => "Signal's config has no database key".to_string(),
            KeyError::KeychainDenied(_) => {
                "Keychain access for \"Signal Safe Storage\" was denied".to_string()
            }
            KeyError::Undecryptable(_) => {
                "Signal's database key could not be unwrapped".to_string()
            }
        }
    }

    /// True when the cause is "not installed" rather than "blocked".
    pub fn is_absent(&self) -> bool {
        matches!(self, KeyError::ConfigMissing(_))
    }

    pub fn is_blocked(&self) -> bool {
        matches!(
            self,
            KeyError::ConfigUnreadable(_) | KeyError::KeychainDenied(_)
        )
    }
}

pub fn signal_support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/Signal")
}

pub fn signal_config_path() -> PathBuf {
    signal_support_dir().join("config.json")
}

/// Resolve the SQLCipher key for the local Signal Desktop profile.
pub fn signal_db_key() -> Result<Secret, KeyError> {
    let path = signal_config_path();
    let raw = read_config(&path)?;
    let key_material = parse_config(&raw)?;
    match key_material {
        KeyMaterial::Plain(hex) => Ok(Secret::new(hex.into_bytes())),
        KeyMaterial::Wrapped(ciphertext) => {
            let password = read_keychain_password(KEYCHAIN_SERVICE)?;
            unwrap_key(password.as_bytes(), &ciphertext)
        }
    }
}

fn read_config(path: &Path) -> Result<String, KeyError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(KeyError::ConfigMissing(path.display().to_string()))
        }
        Err(e) => {
            // macOS reports an FDA block as EPERM; std maps most of
            // those to PermissionDenied but not all filesystems do.
            if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(1) {
                Err(KeyError::ConfigUnreadable(path.display().to_string()))
            } else {
                Err(KeyError::ConfigUnreadable(e.to_string()))
            }
        }
    }
}

enum KeyMaterial {
    /// Legacy: the hex key sits in `config.json` unencrypted.
    Plain(String),
    /// Current: `v10` + AES-128-CBC ciphertext, tag already stripped.
    Wrapped(Vec<u8>),
}

fn parse_config(raw: &str) -> Result<KeyMaterial, KeyError> {
    let v: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| KeyError::Undecryptable(format!("config.json is not JSON: {e}")))?;
    if let Some(k) = v.get("encryptedKey").and_then(|k| k.as_str()) {
        let bytes = from_hex(k)
            .ok_or_else(|| KeyError::Undecryptable("encryptedKey is not hex".to_string()))?;
        if bytes.len() <= VERSION_TAG.len() || &bytes[..VERSION_TAG.len()] != VERSION_TAG {
            return Err(KeyError::Undecryptable(
                "encryptedKey has an unknown version tag".to_string(),
            ));
        }
        return Ok(KeyMaterial::Wrapped(bytes[VERSION_TAG.len()..].to_vec()));
    }
    if let Some(k) = v.get("key").and_then(|k| k.as_str()) {
        return Ok(KeyMaterial::Plain(k.to_string()));
    }
    Err(KeyError::ConfigHasNoKey)
}

/// Shell out to `/usr/bin/security` rather than linking a keychain
/// crate: it is the Apple-shipped, code-signed path to one item, and
/// it keeps the secret out of our argv (`-w` prints it on stdout).
fn read_keychain_password(service: &str) -> Result<Secret, KeyError> {
    let out = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-ws", service])
        .output()
        .map_err(|e| KeyError::KeychainDenied(e.to_string()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(KeyError::KeychainDenied(stderr.trim().to_string()));
    }
    let mut bytes = out.stdout;
    // `security -w` appends a newline.
    while matches!(bytes.last(), Some(b'\n') | Some(b'\r')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(KeyError::KeychainDenied("empty keychain item".to_string()));
    }
    Ok(Secret::new(bytes))
}

/// password + ciphertext → 64-hex SQLCipher key.
pub fn unwrap_key(password: &[u8], ciphertext: &[u8]) -> Result<Secret, KeyError> {
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(KeyError::Undecryptable(
            "ciphertext is not a whole number of AES blocks".to_string(),
        ));
    }
    let mut kek = [0u8; KEK_LEN];
    pbkdf2_hmac_sha1(password, SALT, ITERATIONS, &mut kek);
    let mut buf = ciphertext.to_vec();
    let plain = Aes128CbcDec::new(&kek.into(), &IV.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| KeyError::Undecryptable(e.to_string()))?
        .to_vec();
    for b in kek.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    let secret = Secret::new(plain);
    if secret.as_hex_str().is_none() {
        return Err(KeyError::Undecryptable(
            "unwrapped value is not a 64-character hex key".to_string(),
        ));
    }
    Ok(secret)
}

/// PBKDF2-HMAC-SHA1 (RFC 2898). Hand-rolled over `hmac`/`sha1` to keep
/// the dependency surface of a privacy build small; covered by the
/// RFC 6070 vectors below.
pub fn pbkdf2_hmac_sha1(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    type HmacSha1 = Hmac<Sha1>;
    const H_LEN: usize = 20;
    let mut block_index: u32 = 1;
    let mut written = 0;
    while written < out.len() {
        let mut mac = HmacSha1::new_from_slice(password).expect("HMAC accepts keys of any length");
        mac.update(salt);
        mac.update(&block_index.to_be_bytes());
        let mut u = mac.finalize().into_bytes();
        let mut acc = [0u8; H_LEN];
        acc.copy_from_slice(&u);
        for _ in 1..iterations {
            let mut mac =
                HmacSha1::new_from_slice(password).expect("HMAC accepts keys of any length");
            mac.update(&u);
            u = mac.finalize().into_bytes();
            for (a, b) in acc.iter_mut().zip(u.iter()) {
                *a ^= b;
            }
        }
        let take = (out.len() - written).min(H_LEN);
        out[written..written + take].copy_from_slice(&acc[..take]);
        written += take;
        block_index += 1;
    }
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut};
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    // RFC 6070 test vectors for PBKDF2-HMAC-SHA1.
    #[test]
    fn pbkdf2_matches_rfc6070_one_iteration() {
        let mut out = [0u8; 20];
        pbkdf2_hmac_sha1(b"password", b"salt", 1, &mut out);
        assert_eq!(to_hex(&out), "0c60c80f961f0e71f3a9b524af6012062fe037a6");
    }

    #[test]
    fn pbkdf2_matches_rfc6070_many_iterations() {
        let mut out = [0u8; 20];
        pbkdf2_hmac_sha1(b"password", b"salt", 4096, &mut out);
        assert_eq!(to_hex(&out), "4b007901b765489abead49d926f721d065a429c1");
    }

    #[test]
    fn pbkdf2_spans_multiple_blocks() {
        // dkLen 25 > hLen 20, so block 2 must contribute. RFC 6070's
        // 4-arg vector with c=4096 and dkLen=25.
        let mut out = [0u8; 25];
        pbkdf2_hmac_sha1(
            b"passwordPASSWORDpassword",
            b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
            4096,
            &mut out,
        );
        assert_eq!(
            to_hex(&out),
            "3d2eec4fe41c849b80c8d83662c0e44a8b291a964cf2f07038"
        );
    }

    /// Round-trip: wrap a known hex key the way Chromium does, then
    /// confirm `unwrap_key` returns it.
    #[test]
    fn unwrap_key_round_trips_chromium_wrapping() {
        let db_key = "a".repeat(64);
        let password = b"some-random-keychain-password";
        let mut kek = [0u8; KEK_LEN];
        pbkdf2_hmac_sha1(password, SALT, ITERATIONS, &mut kek);
        let mut buf = vec![0u8; db_key.len() + 16];
        buf[..db_key.len()].copy_from_slice(db_key.as_bytes());
        let ct = Aes128CbcEnc::new(&kek.into(), &IV.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, db_key.len())
            .expect("encrypt");
        let got = unwrap_key(password, ct).expect("unwrap");
        assert_eq!(got.as_hex_str(), Some(db_key.as_str()));
    }

    #[test]
    fn unwrap_key_rejects_wrong_password() {
        let db_key = "b".repeat(64);
        let mut kek = [0u8; KEK_LEN];
        pbkdf2_hmac_sha1(b"right", SALT, ITERATIONS, &mut kek);
        let mut buf = vec![0u8; db_key.len() + 16];
        buf[..db_key.len()].copy_from_slice(db_key.as_bytes());
        let ct = Aes128CbcEnc::new(&kek.into(), &IV.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, db_key.len())
            .expect("encrypt")
            .to_vec();
        assert!(unwrap_key(b"wrong", &ct).is_err());
    }

    #[test]
    fn parse_config_prefers_encrypted_key_and_strips_tag() {
        let raw = r#"{"encryptedKey":"763130aabb"}"#;
        match parse_config(raw).expect("parse") {
            KeyMaterial::Wrapped(ct) => assert_eq!(ct, vec![0xaa, 0xbb]),
            _ => panic!("expected wrapped"),
        }
    }

    #[test]
    fn parse_config_accepts_legacy_plaintext_key() {
        let raw = r#"{"key":"deadbeef"}"#;
        match parse_config(raw).expect("parse") {
            KeyMaterial::Plain(k) => assert_eq!(k, "deadbeef"),
            _ => panic!("expected plain"),
        }
    }

    #[test]
    fn parse_config_rejects_unknown_version_tag() {
        let raw = r#"{"encryptedKey":"763131aabb"}"#;
        assert!(matches!(parse_config(raw), Err(KeyError::Undecryptable(_))));
    }

    #[test]
    fn parse_config_reports_missing_key() {
        assert_eq!(parse_config("{}").err(), Some(KeyError::ConfigHasNoKey));
    }

    #[test]
    fn secret_rejects_non_hex_values() {
        assert!(Secret::new(b"not a key".to_vec()).as_hex_str().is_none());
        assert!(Secret::new(vec![b'a'; 63]).as_hex_str().is_none());
        assert!(Secret::new(vec![b'a'; 64]).as_hex_str().is_some());
    }

    #[test]
    fn hex_decoder_rejects_malformed_input() {
        assert_eq!(from_hex("00ff"), Some(vec![0, 255]));
        assert_eq!(from_hex("0"), None);
        assert_eq!(from_hex("zz"), None);
    }
}
