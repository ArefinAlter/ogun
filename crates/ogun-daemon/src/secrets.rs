//! Ogun's secret store: one `0600` file per name under `secrets/` (constraint 4).
//!
//! Values are read only to hand them to an operator's stdin; they are zeroized after use and never
//! logged, evented, put in lineage or returned by any method.

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

/// The secret directory.
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    /// Opens (creates) the directory.
    ///
    /// # Errors
    ///
    /// `api.unavailable` on I/O failure.
    pub fn open(dir: PathBuf) -> ApiResult<Self> {
        private_dir(&dir)?;
        Ok(Self { dir })
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
        std::fs::remove_file(self.dir.join(name))
            .map_err(|_| ApiError::new("secret.not_found", "no such secret"))
    }

    /// Every name, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
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
        std::fs::read_to_string(self.dir.join(name))
            .ok()
            .map(Zeroizing::new)
    }
}
