//! Secret values on a desktop (CLAUDE.md constraint 4; the standalone brief's provider-key rules).
//!
//! * **Operating system store.** Windows Credential Manager or the Linux Secret Service (GNOME Keyring, KWallet),
//!   through `keyring-core`. Probed once with a throwaway entry: a machine without a running service reports
//!   [`Kind::Unavailable`], it does not fail later at the first real key.
//! * **Encrypted file.** XChaCha20-Poly1305 under a key from Argon2id over a passphrase the person gives. For
//!   headless Linux and anywhere the first option is absent.
//! * **Names are listed, values are not.** Credential stores cannot enumerate, so a small index of names sits
//!   beside the store (names only). A value leaves this crate only as `Zeroizing<String>`, and no type here
//!   implements `Serialize` or prints a value.

#![forbid(unsafe_code)]

mod file;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub use file::EncryptedFile;

/// Why a store operation failed. Messages never contain a value.
#[derive(Debug, thiserror::Error)]
pub enum KeystoreError {
    /// No store is usable: the machine has no credential service and no passphrase was given.
    #[error("no secret store is available: {0}")]
    Unavailable(String),
    /// The encrypted file is locked or the passphrase is wrong.
    #[error("the encrypted store could not be opened (wrong passphrase?)")]
    Locked,
    /// The name is not a valid secret name.
    #[error("secret names are 1 to 128 of [A-Za-z0-9._-]")]
    BadName,
    /// The platform store refused.
    #[error("the credential store refused: {0}")]
    Platform(String),
    /// An I/O error on the index or file.
    #[error("store file: {0}")]
    Io(String),
}

/// Result alias.
pub type KeystoreResult<T> = Result<T, KeystoreError>;

/// Which store holds the values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Windows Credential Manager, the Secret Service or the Keychain.
    Os,
    /// A passphrase-encrypted file.
    EncryptedFile,
    /// Neither; the caller decides what to do (the daemon falls back to its owner-only files and says so).
    Unavailable,
}

/// How to choose the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The OS store if it works, else the encrypted file when a passphrase is given, else unavailable.
    Auto,
    /// Only the OS store.
    Os,
    /// Only the encrypted file.
    File,
}

/// A valid secret name: 1 to 128 of `[A-Za-z0-9._-]`, not starting with a dot.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

enum Backend {
    Os { service: String },
    File(EncryptedFile),
    None { reason: String },
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Os { service } => write!(f, "Os({service})"),
            Self::File(_) => f.write_str("EncryptedFile"),
            Self::None { reason } => write!(f, "None({reason})"),
        }
    }
}

/// The store.
#[derive(Debug, Clone)]
pub struct Keystore {
    backend: Arc<Backend>,
    index: PathBuf,
}

#[cfg(windows)]
fn install_os_store() -> Result<(), String> {
    let store = windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
    keyring_core::set_default_store(store);
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_os_store() -> Result<(), String> {
    let store = zbus_secret_service_keyring_store::Store::new().map_err(|e| e.to_string())?;
    keyring_core::set_default_store(store);
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn install_os_store() -> Result<(), String> {
    Err("no OS credential store is wired for this platform in this build".into())
}

/// Installs the OS store once per process and proves it works with a throwaway entry.
fn probe_os(service: &str) -> Result<(), String> {
    static INSTALLED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(install_os_store).clone()?;
    let entry = keyring_core::Entry::new(service, "ogun-probe").map_err(|e| e.to_string())?;
    entry.set_password("probe").map_err(|e| e.to_string())?;
    let back = entry.get_password().map_err(|e| e.to_string())?;
    let _ = entry.delete_credential();
    if back == "probe" {
        Ok(())
    } else {
        Err("the credential store did not return what was written".into())
    }
}

impl Keystore {
    /// Opens the store for `service` (for example `Ogun studio 1a2b...`), keeping the index of names in `dir`.
    ///
    /// # Errors
    ///
    /// [`KeystoreError::Io`] when `dir` cannot be created; [`KeystoreError::Unavailable`] for [`Mode::Os`] or
    /// [`Mode::File`] when that store cannot be used. [`Mode::Auto`] never fails for lack of a store: it yields
    /// [`Kind::Unavailable`].
    pub fn open(
        service: &str,
        dir: &Path,
        mode: Mode,
        passphrase: Option<&Zeroizing<String>>,
    ) -> KeystoreResult<Self> {
        std::fs::create_dir_all(dir).map_err(|e| KeystoreError::Io(e.kind().to_string()))?;
        let index = dir.join("names.json");
        let os = || Backend::Os {
            service: service.to_owned(),
        };
        let backend = match mode {
            Mode::Os => probe_os(service)
                .map(|()| os())
                .map_err(KeystoreError::Unavailable)?,
            Mode::File => Backend::File(Self::file_store(dir, passphrase)?),
            Mode::Auto => match probe_os(service) {
                Ok(()) => os(),
                Err(reason) => match passphrase {
                    Some(_) => Backend::File(Self::file_store(dir, passphrase)?),
                    None => {
                        tracing::info!(%reason, "no OS credential store; secrets stay in the daemon's owner-only files");
                        Backend::None { reason }
                    }
                },
            },
        };
        Ok(Self {
            backend: Arc::new(backend),
            index,
        })
    }

    fn file_store(
        dir: &Path,
        passphrase: Option<&Zeroizing<String>>,
    ) -> KeystoreResult<EncryptedFile> {
        let p = passphrase.ok_or_else(|| {
            KeystoreError::Unavailable("an encrypted store needs a passphrase".into())
        })?;
        EncryptedFile::open(&dir.join("vault.json"), p)
    }

    /// Which store this is.
    #[must_use]
    pub fn kind(&self) -> Kind {
        match &*self.backend {
            Backend::Os { .. } => Kind::Os,
            Backend::File(_) => Kind::EncryptedFile,
            Backend::None { .. } => Kind::Unavailable,
        }
    }

    /// Why the store is unavailable, when it is.
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        match &*self.backend {
            Backend::None { reason } => Some(reason),
            _ => None,
        }
    }

    fn read_index(&self) -> Vec<String> {
        std::fs::read(&self.index)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
            .unwrap_or_default()
    }

    fn write_index(&self, names: &[String]) -> KeystoreResult<()> {
        let bytes = serde_json::to_vec(names).map_err(|e| KeystoreError::Io(e.to_string()))?;
        let tmp = self.index.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|e| KeystoreError::Io(e.kind().to_string()))?;
        std::fs::rename(&tmp, &self.index).map_err(|e| KeystoreError::Io(e.kind().to_string()))
    }

    fn entry(service: &str, name: &str) -> KeystoreResult<keyring_core::Entry> {
        keyring_core::Entry::new(service, &format!("secret:{name}"))
            .map_err(|e| KeystoreError::Platform(e.to_string()))
    }

    /// Stores (replaces) a value.
    ///
    /// # Errors
    ///
    /// [`KeystoreError::BadName`], [`KeystoreError::Unavailable`] or the store's refusal.
    pub fn set(&self, name: &str, value: &Zeroizing<String>) -> KeystoreResult<()> {
        if !valid_name(name) {
            return Err(KeystoreError::BadName);
        }
        match &*self.backend {
            Backend::Os { service } => Self::entry(service, name)?
                .set_password(value)
                .map_err(|e| KeystoreError::Platform(e.to_string()))?,
            Backend::File(f) => f.set(name, value)?,
            Backend::None { reason } => return Err(KeystoreError::Unavailable(reason.clone())),
        }
        let mut names = self.read_index();
        if !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
            names.sort();
            self.write_index(&names)?;
        }
        Ok(())
    }

    /// Reads a value; `None` when there is none.
    ///
    /// # Errors
    ///
    /// [`KeystoreError::Unavailable`] or the store's refusal.
    pub fn get(&self, name: &str) -> KeystoreResult<Option<Zeroizing<String>>> {
        if !valid_name(name) {
            return Ok(None);
        }
        match &*self.backend {
            Backend::Os { service } => match Self::entry(service, name)?.get_password() {
                Ok(v) => Ok(Some(Zeroizing::new(v))),
                Err(keyring_core::Error::NoEntry) => Ok(None),
                Err(e) => Err(KeystoreError::Platform(e.to_string())),
            },
            Backend::File(f) => f.get(name),
            Backend::None { reason } => Err(KeystoreError::Unavailable(reason.clone())),
        }
    }

    /// Deletes a value; `false` when there was none.
    ///
    /// # Errors
    ///
    /// [`KeystoreError::Unavailable`] or the store's refusal.
    pub fn delete(&self, name: &str) -> KeystoreResult<bool> {
        if !valid_name(name) {
            return Ok(false);
        }
        let existed = match &*self.backend {
            Backend::Os { service } => match Self::entry(service, name)?.delete_credential() {
                Ok(()) => true,
                Err(keyring_core::Error::NoEntry) => false,
                Err(e) => return Err(KeystoreError::Platform(e.to_string())),
            },
            Backend::File(f) => f.delete(name)?,
            Backend::None { reason } => return Err(KeystoreError::Unavailable(reason.clone())),
        };
        let mut names = self.read_index();
        let before = names.len();
        names.retain(|n| n != name);
        if names.len() != before {
            self.write_index(&names)?;
        }
        Ok(existed)
    }

    /// Every stored name, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.read_index()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANARY: &str = "OGUN-CANARY-keystore-value-0001";

    #[test]
    fn names_are_validated() {
        assert!(valid_name("anthropic_api_key"));
        assert!(!valid_name(""));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(&"x".repeat(129)));
    }

    #[test]
    fn the_encrypted_file_round_trips_and_never_holds_the_value_in_clear() {
        let dir = tempfile::tempdir().unwrap();
        let pass = Zeroizing::new("correct horse".to_owned());
        let ks = Keystore::open("test", dir.path(), Mode::File, Some(&pass)).unwrap();
        assert_eq!(ks.kind(), Kind::EncryptedFile);
        ks.set("k", &Zeroizing::new(CANARY.to_owned())).unwrap();
        assert_eq!(ks.get("k").unwrap().unwrap().as_str(), CANARY);
        assert_eq!(ks.names(), vec!["k".to_owned()]);
        for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(CANARY),
                "{} holds the value",
                entry.path().display()
            );
        }
        // A second open with the right passphrase sees it; the wrong one is refused.
        let again = Keystore::open("test", dir.path(), Mode::File, Some(&pass)).unwrap();
        assert_eq!(again.get("k").unwrap().unwrap().as_str(), CANARY);
        let wrong = Zeroizing::new("battery staple".to_owned());
        assert!(matches!(
            Keystore::open("test", dir.path(), Mode::File, Some(&wrong)),
            Err(KeystoreError::Locked)
        ));
        assert!(ks.delete("k").unwrap());
        assert!(!ks.delete("k").unwrap());
        assert!(ks.get("k").unwrap().is_none());
    }

    #[test]
    fn auto_without_a_store_or_passphrase_is_unavailable_not_an_error() {
        // Only meaningful where no OS store works (a container); where one does, the kind is `Os`.
        let dir = tempfile::tempdir().unwrap();
        let ks = Keystore::open("ogun-test-auto", dir.path(), Mode::Auto, None).unwrap();
        if ks.kind() == Kind::Unavailable {
            assert!(matches!(
                ks.set("k", &Zeroizing::new("v".into())),
                Err(KeystoreError::Unavailable(_))
            ));
        }
    }

    #[test]
    fn the_os_store_round_trips_when_there_is_one() {
        let dir = tempfile::tempdir().unwrap();
        let Ok(ks) = Keystore::open("Ogun keystore test", dir.path(), Mode::Os, None) else {
            return;
        };
        ks.set("probe-test", &Zeroizing::new(CANARY.to_owned()))
            .unwrap();
        assert_eq!(ks.get("probe-test").unwrap().unwrap().as_str(), CANARY);
        assert!(ks.delete("probe-test").unwrap());
        assert!(ks.get("probe-test").unwrap().is_none());
    }
}
