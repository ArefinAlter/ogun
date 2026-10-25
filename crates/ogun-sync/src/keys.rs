//! The edge identity key (O-11 section 2): Ed25519, generated once on the edge, stored in a `0600`
//! file and zeroized in memory. The public key travels as base64url without padding (43
//! characters); a person enrols it on the hub with `edges.enroll`, which binds it to an `edg_` id.
//!
//! The key file holds one line: `ogun-edge-key-v1 <base64url of the 32-byte seed>`. The secret is
//! never logged, never serialised and never part of an error message.

use std::fmt;
use std::io::Write;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

const KEY_FILE_PREFIX: &str = "ogun-edge-key-v1 ";

/// Key generation, storage and loading failures. Never carries key material.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// `keygen` refuses to overwrite an existing key.
    #[error("the key file already exists; refusing to overwrite it")]
    Exists,
    /// The file could not be read or written.
    #[error("cannot read or write the key file: {0}")]
    Io(std::io::ErrorKind),
    /// The file is not an Ogun edge key.
    #[error("the key file is not an Ogun edge key")]
    Malformed,
    /// The file is readable or writable by other users.
    #[error("the key file is accessible to other users; it must be mode 0600")]
    Permissions,
    /// The OS has no random source.
    #[error("no OS random source")]
    Random,
}

/// The edge's signing key. No `Serialize`, no `Clone`; `Debug` shows only the public key. The
/// inner key zeroizes on drop (ed25519-dalek's `zeroize` feature).
pub struct EdgeKey {
    signing: SigningKey,
}

impl fmt::Debug for EdgeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EdgeKey")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

impl EdgeKey {
    /// A fresh key from the OS random source.
    ///
    /// # Errors
    ///
    /// [`KeyError::Random`] without an entropy source.
    pub fn generate() -> Result<Self, KeyError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut seed[..]).map_err(|_| KeyError::Random)?;
        Ok(Self::from_seed(&seed))
    }

    /// A key from a 32-byte seed (tests, or a key imported by a person).
    #[must_use]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(seed),
        }
    }

    /// The public key, base64url without padding.
    #[must_use]
    pub fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.signing.verifying_key().as_bytes())
    }

    /// Signs `message`; the signature is base64url without padding (86 characters).
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.signing.sign(message).to_bytes())
    }

    /// Writes the key to a new file with mode `0600` (the parent directory is created `0700`).
    ///
    /// # Errors
    ///
    /// [`KeyError::Exists`] when the file exists, [`KeyError::Io`] otherwise.
    pub fn write_new(&self, path: &Path) -> Result<(), KeyError> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => KeyError::Exists,
            other => KeyError::Io(other),
        })?;
        let seed = Zeroizing::new(self.signing.to_bytes());
        let mut line = Zeroizing::new(String::with_capacity(64));
        line.push_str(KEY_FILE_PREFIX);
        URL_SAFE_NO_PAD.encode_string(&seed[..], &mut line);
        line.push('\n');
        file.write_all(line.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| KeyError::Io(e.kind()))
    }

    /// Loads a key written by [`EdgeKey::write_new`].
    ///
    /// # Errors
    ///
    /// [`KeyError::Permissions`] on Unix when group or others can access the file;
    /// [`KeyError::Malformed`] or [`KeyError::Io`] otherwise.
    pub fn load(path: &Path) -> Result<Self, KeyError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(path).map_err(|e| KeyError::Io(e.kind()))?;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(KeyError::Permissions);
            }
        }
        let text =
            Zeroizing::new(std::fs::read_to_string(path).map_err(|e| KeyError::Io(e.kind()))?);
        let body = text
            .trim_end()
            .strip_prefix(KEY_FILE_PREFIX)
            .ok_or(KeyError::Malformed)?;
        let bytes = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(body)
                .map_err(|_| KeyError::Malformed)?,
        );
        let mut seed = Zeroizing::new([0u8; 32]);
        if bytes.len() != seed.len() {
            return Err(KeyError::Malformed);
        }
        seed.copy_from_slice(&bytes);
        Ok(Self::from_seed(&seed))
    }
}

/// Parses an enrolled public key (base64url, 32 bytes). `None` when malformed or not a valid
/// curve point.
#[must_use]
pub fn parse_public_key(text: &str) -> Option<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD.decode(text.trim()).ok()?;
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// Creates a directory (and parents) readable only by the owner on Unix.
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), KeyError> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|e| KeyError::Io(e.kind()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_load_round_trip_and_no_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync").join("edge.key");
        let key = EdgeKey::generate().unwrap();
        key.write_new(&path).unwrap();
        assert!(matches!(key.write_new(&path), Err(KeyError::Exists)));
        let loaded = EdgeKey::load(&path).unwrap();
        assert_eq!(loaded.public_key(), key.public_key());
        assert_eq!(key.public_key().len(), 43);
        assert!(parse_public_key(&key.public_key()).is_some());
        let dbg = format!("{key:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        let secret = text.trim_end().strip_prefix(KEY_FILE_PREFIX).unwrap();
        assert!(!dbg.contains(secret), "Debug must not print the secret");
    }

    #[cfg(unix)]
    #[test]
    fn group_readable_key_files_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge.key");
        EdgeKey::generate().unwrap().write_new(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(EdgeKey::load(&path), Err(KeyError::Permissions)));
    }
}
