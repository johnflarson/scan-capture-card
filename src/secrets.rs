//! Encryption for the secrets kept in the database: people's own vision-LLM
//! API keys. They are sealed with XChaCha20-Poly1305 under the server's
//! key: `MTG_SCAN_SECRET_KEY` (64 hex digits, e.g. in `.env`) when set,
//! otherwise `secret.key` in the data folder, created on first use. A copy
//! of the database alone doesn't reveal the keys; without the server's key
//! they can't be read, and people enter them again.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

pub const KEY_VAR: &str = "MTG_SCAN_SECRET_KEY";
/// Sealed values start with this, so the format can change later.
const PREFIX: &str = "v1:";

pub struct SecretBox {
    cipher: XChaCha20Poly1305,
}

impl SecretBox {
    /// The server's key from `MTG_SCAN_SECRET_KEY`, else `secret.key` in the
    /// data folder (created when missing).
    pub fn open() -> Result<Self> {
        match std::env::var(KEY_VAR).ok().filter(|v| !v.trim().is_empty()) {
            Some(hex) => {
                Ok(Self::new(&parse_hex(hex.trim()).with_context(|| {
                    format!("{KEY_VAR} is not 64 hex digits")
                })?))
            }
            None => Self::from_file(&key_file()),
        }
    }

    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: XChaCha20Poly1305::new(key.into()),
        }
    }

    fn from_file(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::new(&parse_hex(text.trim()).with_context(|| {
                format!("{} is not a key (64 hex digits)", path.display())
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key).map_err(|e| anyhow!("no secure random source: {e}"))?;
                write_private(path, &hex(&key))?;
                Ok(Self::new(&key))
            }
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    pub fn seal(&self, plain: &str) -> Result<String> {
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(|e| anyhow!("no secure random source: {e}"))?;
        let sealed = self
            .cipher
            .encrypt(XNonce::from_slice(&nonce), plain.as_bytes())
            .map_err(|_| anyhow!("encryption failed"))?;
        let mut bytes = nonce.to_vec();
        bytes.extend_from_slice(&sealed);
        Ok(format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        ))
    }

    pub fn unseal(&self, sealed: &str) -> Result<String> {
        let data = sealed.strip_prefix(PREFIX).context("not a sealed value")?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(data)
            .context("not a sealed value")?;
        if bytes.len() < 24 {
            bail!("not a sealed value");
        }
        let (nonce, sealed) = bytes.split_at(24);
        let plain = self
            .cipher
            .decrypt(XNonce::from_slice(nonce), sealed)
            .map_err(|_| {
                anyhow!(
                    "can't be read with this server's key (was {KEY_VAR} or secret.key changed?)"
                )
            })?;
        String::from_utf8(plain).context("not text")
    }
}

/// `secret.key` in the data folder.
pub fn key_file() -> PathBuf {
    crate::config::data_dir().join("secret.key")
}

/// Write a file only its owner can read (on Unix; Windows keeps the user
/// folder's permissions).
fn write_private(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    std::io::Write::write_all(&mut file, format!("{text}\n").as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hex(text: &str) -> Result<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected 64 hex digits");
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_values_open_with_the_same_key_only() {
        let a = SecretBox::new(&[7; 32]);
        let sealed = a.seal("sk-or-v1-secret").unwrap();
        assert!(sealed.starts_with(PREFIX));
        assert!(!sealed.contains("secret"));
        assert_ne!(sealed, a.seal("sk-or-v1-secret").unwrap(), "fresh nonce");
        assert_eq!(a.unseal(&sealed).unwrap(), "sk-or-v1-secret");
        let b = SecretBox::new(&[8; 32]);
        assert!(b.unseal(&sealed).is_err());
        assert!(a.unseal("v1:short").is_err());
        assert!(a.unseal("plain").is_err());
    }

    #[test]
    fn the_key_file_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("secret.key");
        let first = SecretBox::from_file(&path).unwrap();
        let sealed = first.seal("x").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim().len(), 64);
        let again = SecretBox::from_file(&path).unwrap();
        assert_eq!(again.unseal(&sealed).unwrap(), "x");
    }

    #[test]
    fn hex_keys_parse() {
        let key = [0xab; 32];
        assert_eq!(parse_hex(&hex(&key)).unwrap(), key);
        assert!(parse_hex("abc").is_err());
        assert!(parse_hex(&"zz".repeat(32)).is_err());
    }
}
