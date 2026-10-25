//! The bridge's own configuration, `<state_dir>/ingest.json` (O-10 section 2.2).
//!
//! It holds what the daemon must not hold: bind addresses, broker and device addresses that
//! override a node's configuration, and the paths of `0600` files with secrets (broker passwords,
//! HTTP bearer tokens, TLS client keys). Secret values are read when a source starts, kept in
//! zeroizing memory, and never logged, serialised or put in an error message.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zeroize::Zeroizing;

/// `ingest.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestConfig {
    /// Only these workflows; every enabled workflow with a stream source when empty.
    #[serde(default)]
    pub workflows: Vec<String>,
    /// Where the per-source ingest logs live; `<state_dir>/ingest` when absent.
    #[serde(default)]
    pub log_dir: Option<PathBuf>,
    /// Group commit: at most this many records per log transaction.
    #[serde(default = "defaults::batch_max_records")]
    pub batch_max_records: usize,
    /// Group commit: at most this long between the first record of a batch and its commit.
    #[serde(default = "defaults::batch_max_delay_ms")]
    pub batch_max_delay_ms: u64,
    /// How often the bridge re-reads the workflow list from `ogund`.
    #[serde(default = "defaults::refresh_ms")]
    pub refresh_ms: u64,
    /// How often each source logs its status line.
    #[serde(default = "defaults::status_interval_ms")]
    pub status_interval_ms: u64,
    /// `secret_ref` name to the `0600` file holding the secret.
    #[serde(default)]
    pub secrets: BTreeMap<String, PathBuf>,
    /// Per source, keyed `<workflow>/<node>`.
    #[serde(default)]
    pub sources: BTreeMap<String, SourceOverrides>,
}

mod defaults {
    pub const fn batch_max_records() -> usize {
        1_000
    }
    pub const fn batch_max_delay_ms() -> u64 {
        200
    }
    pub const fn refresh_ms() -> u64 {
        30_000
    }
    pub const fn status_interval_ms() -> u64 {
        60_000
    }
}

/// Bridge-side settings of one source. Every member is optional; what a member names wins over
/// the node's configuration in the workflow.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOverrides {
    /// MQTT and LoRaWAN: broker URL, `mqtt://host:port` or `mqtts://host:port`.
    pub broker: Option<String>,
    /// MQTT and LoRaWAN: user name.
    pub username: Option<String>,
    /// MQTT and LoRaWAN: file with the password or API key (`0600`).
    pub password_file: Option<PathBuf>,
    /// MQTT and LoRaWAN: trust roots (PEM) instead of the platform's.
    pub ca_file: Option<PathBuf>,
    /// MQTT and LoRaWAN: client certificate chain (PEM) for mutual TLS.
    pub client_cert_file: Option<PathBuf>,
    /// MQTT and LoRaWAN: client private key (PEM, `0600`) for mutual TLS.
    pub client_key_file: Option<PathBuf>,
    /// MQTT and LoRaWAN: client id; stable, so the broker keeps the persistent session.
    pub client_id: Option<String>,
    /// HTTP push: listen address; `127.0.0.1:8740` when absent.
    pub bind: Option<String>,
    /// HTTP push: file of accepted bearer tokens, one per line (`0600`).
    pub token_file: Option<PathBuf>,
    /// HTTP push: largest request body; 1 MiB when absent, at most 16 MiB.
    pub max_body_bytes: Option<u64>,
    /// Modbus: device id to `host:port`.
    #[serde(default)]
    pub devices: BTreeMap<String, String>,
    /// File tail: the file to follow.
    pub path: Option<PathBuf>,
}

/// Configuration problems. Never carry a secret value.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `ingest.json` is malformed.
    #[error("ingest.json: {0}")]
    Parse(String),
    /// A file could not be read.
    #[error("cannot read {what}: {kind:?}")]
    Io {
        /// What the file is for.
        what: String,
        /// Why.
        kind: std::io::ErrorKind,
    },
    /// A secret file is accessible to other users.
    #[error("{0} is accessible to other users; it must be mode 0600")]
    Permissions(String),
    /// A setting is missing or invalid.
    #[error("{0}")]
    Invalid(String),
}

impl IngestConfig {
    /// Reads `ingest.json`; a missing file is the default configuration.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when the file exists but cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default_values()),
            Err(e) => {
                return Err(ConfigError::Io {
                    what: "ingest.json".into(),
                    kind: e.kind(),
                });
            }
        };
        serde_json::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    fn default_values() -> Self {
        Self {
            batch_max_records: defaults::batch_max_records(),
            batch_max_delay_ms: defaults::batch_max_delay_ms(),
            refresh_ms: defaults::refresh_ms(),
            status_interval_ms: defaults::status_interval_ms(),
            ..Self::default()
        }
    }

    /// The overrides of one source (empty when none are configured).
    #[must_use]
    pub fn overrides(&self, workflow: &str, node: &str) -> SourceOverrides {
        self.sources
            .get(&format!("{workflow}/{node}"))
            .cloned()
            .unwrap_or_default()
    }
}

/// A secret read from a file: zeroized on drop, no `Serialize`, redacted `Debug`.
pub struct Secret(Zeroizing<String>);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Secret {
    /// The value, for the one call that needs it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Checks that only the owner can access `path` (Unix). Other platforms rely on the profile
/// directory's ACL.
///
/// # Errors
///
/// [`ConfigError::Permissions`] or [`ConfigError::Io`].
pub fn check_private(path: &Path, what: &str) -> Result<(), ConfigError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| ConfigError::Io {
            what: what.to_owned(),
            kind: e.kind(),
        })?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(ConfigError::Permissions(what.to_owned()));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, what);
    }
    Ok(())
}

/// Reads a `0600` file into zeroizing memory.
///
/// # Errors
///
/// [`ConfigError::Permissions`] or [`ConfigError::Io`].
pub fn read_private(path: &Path, what: &str) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    check_private(path, what)?;
    std::fs::read(path)
        .map(Zeroizing::new)
        .map_err(|e| ConfigError::Io {
            what: what.to_owned(),
            kind: e.kind(),
        })
}

/// Reads a one-value secret file (trailing newline removed).
///
/// # Errors
///
/// As [`read_private`]; [`ConfigError::Invalid`] when the file is empty or not UTF-8.
pub fn read_secret(path: &Path, what: &str) -> Result<Secret, ConfigError> {
    let bytes = read_private(path, what)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ConfigError::Invalid(format!("{what} is not UTF-8")))?;
    let value = text.trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        return Err(ConfigError::Invalid(format!("{what} is empty")));
    }
    Ok(Secret(Zeroizing::new(value.to_owned())))
}

/// Reads a token file: one token per line; blank lines and lines starting with `#` are skipped.
///
/// # Errors
///
/// As [`read_private`]; [`ConfigError::Invalid`] when no token is in the file.
pub fn read_tokens(path: &Path, what: &str) -> Result<Vec<Secret>, ConfigError> {
    let bytes = read_private(path, what)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ConfigError::Invalid(format!("{what} is not UTF-8")))?;
    let tokens: Vec<Secret> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| Secret(Zeroizing::new(l.to_owned())))
        .collect();
    if tokens.is_empty() {
        return Err(ConfigError::Invalid(format!("{what} holds no token")));
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let c = IngestConfig::load(&dir.path().join("ingest.json")).unwrap();
        assert_eq!(c.batch_max_records, 1_000);
        assert_eq!(c.batch_max_delay_ms, 200);
    }

    #[test]
    fn unknown_members_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ingest.json");
        std::fs::write(&p, r#"{"sources": {"w/n": {"pasword_file": "/x"}}}"#).unwrap();
        assert!(IngestConfig::load(&p).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn secrets_need_owner_only_files_and_never_print() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pw");
        std::fs::write(&p, "ZEUS-CANARY-mqtt\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            read_secret(&p, "password_file"),
            Err(ConfigError::Permissions(_))
        ));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let s = read_secret(&p, "password_file").unwrap();
        assert_eq!(s.expose(), "ZEUS-CANARY-mqtt");
        assert!(!format!("{s:?}").contains("CANARY"));
    }
}
