//! Bridge configuration, as it appears in `ogund`'s `config.json` under `zeus_bridge` (O-08 section 4).
//!
//! It holds paths only. The credential itself stays in the file Zeus wrote; Ogun never copies it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default per-request deadline.
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

fn default_timeout() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_MS
}

fn default_label() -> String {
    "ogun imports".to_owned()
}

/// How `ogund` reaches `zeusd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    /// The service credential file `zeus clients add --kind service --name ogun` reported, for example
    /// `~/.local/state/zeus/credentials/ogun.token`.
    pub credential_file: PathBuf,
    /// Zeus's state directory (where `ipc.secret` and `zeusd.json` are). Default: the directory two levels above
    /// `credential_file`, which is where Zeus writes credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zeus_state_dir: Option<PathBuf>,
    /// The endpoint, when it cannot be found from `zeusd.json` or computed (a socket path or a pipe name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Ogun's import directory, registered with `files.roots.register`. It must lie inside one of the user's Zeus
    /// write roots and must not be one itself (Zeus 05 section 9.6).
    pub import_root: PathBuf,
    /// The root's label in Zeus.
    #[serde(default = "default_label")]
    pub root_label: String,
    /// Per-request deadline (`jobs.wait` adds its own timeout).
    #[serde(default = "default_timeout")]
    pub request_timeout_ms: u64,
}

impl BridgeConfig {
    /// A configuration with defaults.
    #[must_use]
    pub fn new(credential_file: PathBuf, import_root: PathBuf) -> Self {
        Self {
            credential_file,
            zeus_state_dir: None,
            endpoint: None,
            import_root,
            root_label: default_label(),
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        }
    }

    /// The Zeus state directory: configured, or derived from the credential file's location.
    #[must_use]
    pub fn state_dir(&self) -> Option<PathBuf> {
        self.zeus_state_dir.clone().or_else(|| {
            self.credential_file
                .parent()
                .and_then(|credentials| credentials.parent())
                .map(PathBuf::from)
        })
    }
}
