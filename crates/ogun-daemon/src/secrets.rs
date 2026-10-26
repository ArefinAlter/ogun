//! Ogun's secret store (constraint 4): the operating system's credential store when a supervising desktop app
//! started the daemon and the machine has one, otherwise one `0600` file per name under `secrets/`.
//!
//! Values are read only to hand them to an operator's stdin (or a provider request); they are zeroized after use
//! and never logged, evented, put in lineage or returned by any method.

use std::path::PathBuf;

use ogun_api::{ApiError, ApiResult};
use zeroize::Zeroizing;

use crate::creds::{private_dir, write_private};

/// A valid secret name: 1 to 128 of `[A-Za-z0-9._-]`, not starting with a dot.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Where the values live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    /// Owner-only files in the state directory.
    Files,
    /// The OS credential store (Windows Credential Manager, the Secret Service).
    Os,
    /// A passphrase-encrypted file.
    EncryptedFile,
}

/// The secret directory, or the credential store that replaces it.
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
    keystore: Option<ogun_keystore::Keystore>,
    note: Option<String>,
}

impl SecretStore {
    /// Opens (creates) the directory.
    ///
    /// # Errors
    ///
    /// `api.unavailable` on I/O failure.
    pub fn open(dir: PathBuf) -> ApiResult<Self> {
        private_dir(&dir)?;
        Ok(Self {
            dir,
            keystore: None,
            note: None,
        })
    }

    /// Opens the OS credential store for this state directory when the machine has one (the encrypted file
    /// when a passphrase is given), and the owner-only files otherwise. `service` names the store entries.
    ///
    /// # Errors
    ///
    /// `api.unavailable` on I/O failure.
    pub fn open_preferring_os(
        dir: PathBuf,
        index_dir: PathBuf,
        service: &str,
        passphrase: Option<&Zeroizing<String>>,
    ) -> ApiResult<Self> {
        let mut me = Self::open(dir)?;
        match ogun_keystore::Keystore::open(
            service,
            &index_dir,
            ogun_keystore::Mode::Auto,
            passphrase,
        ) {
            Ok(ks) if ks.kind() != ogun_keystore::Kind::Unavailable => me.keystore = Some(ks),
            Ok(ks) => {
                me.note = ks
                    .unavailable_reason()
                    .map(|r| format!("no OS credential store: {r}"));
            }
            Err(e) => me.note = Some(e.to_string()),
        }
        Ok(me)
    }

    /// Where the values are, and a sentence when that is not what was wanted.
    #[must_use]
    pub fn kind(&self) -> (StoreKind, Option<&str>) {
        let kind = match self.keystore.as_ref().map(ogun_keystore::Keystore::kind) {
            Some(ogun_keystore::Kind::Os) => StoreKind::Os,
            Some(ogun_keystore::Kind::EncryptedFile) => StoreKind::EncryptedFile,
            _ => StoreKind::Files,
        };
        (kind, self.note.as_deref())
    }

    /// Stores a value.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` for a bad name, or an I/O error.
    pub fn put(&self, name: &str, value: &Zeroizing<String>) -> ApiResult<()> {
        if !valid_name(name) {
            return Err(ApiError::invalid_params(
                "secret names are 1 to 128 of [A-Za-z0-9._-]",
            ));
        }
        if let Some(ks) = &self.keystore {
            return ks
                .set(name, value)
                .map_err(|e| ApiError::unavailable(e.to_string()));
        }
        write_private(&self.dir.join(name), value.as_bytes())
    }

    /// Deletes a value.
    ///
    /// # Errors
    ///
    /// `secret.not_found`.
    pub fn delete(&self, name: &str) -> ApiResult<()> {
        if !valid_name(name) {
            return Err(ApiError::new("secret.not_found", "no such secret"));
        }
        if let Some(ks) = &self.keystore {
            return match ks.delete(name) {
                Ok(true) => Ok(()),
                Ok(false) => Err(ApiError::new("secret.not_found", "no such secret")),
                Err(e) => Err(ApiError::unavailable(e.to_string())),
            };
        }
        std::fs::remove_file(self.dir.join(name))
            .map_err(|_| ApiError::new("secret.not_found", "no such secret"))
    }

    /// Every name, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        if let Some(ks) = &self.keystore {
            return ks.names();
        }
        let mut out: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| valid_name(n))
            .collect();
        out.sort();
        out
    }
}

impl ogun_ops::Secrets for SecretStore {
    fn resolve(&self, name: &str) -> Option<Zeroizing<String>> {
        if !valid_name(name) {
            return None;
        }
        if let Some(ks) = &self.keystore {
            return ks.get(name).ok().flatten();
        }
        std::fs::read_to_string(self.dir.join(name))
            .ok()
            .map(Zeroizing::new)
    }
}
