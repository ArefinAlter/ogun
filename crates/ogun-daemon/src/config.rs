//! `config.json` in the state directory. Every member has a default; a missing file is the defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{DaemonError, DaemonResult};

/// The daemon's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Fire cron and interval triggers. Off for edge profiles that only ingest.
    pub scheduler_enabled: bool,
    /// Attempts running at once across all runs.
    pub max_parallel_attempts: u32,
    /// Executables whose processes count as trust surfaces besides the `ogun` and `ogun-studio`
    /// files next to `ogund` (O-15 section 4.4).
    pub trusted_binaries: Vec<PathBuf>,
    /// Whether `model` operators may run (constraint 6).
    pub model_access: bool,
    /// Directories searched for operator manifests.
    pub operator_dirs: Vec<PathBuf>,
    /// Let child operators run without an OS sandbox where none exists (development only).
    pub allow_unsandboxed: bool,
    /// How long an approval waits before it expires (a denial).
    pub approval_ttl_ms: u64,
    /// Approvals pending at once (Zeus ADR 0007).
    pub max_pending_approvals: u32,
    /// Decisions accepted per minute (Zeus ADR 0007).
    pub decisions_per_minute: u32,
    /// Accept WebAuthn decisions relayed by a bridge (ADR 0010: off until the security review).
    pub remote_approvals: bool,
    /// Artifact store quota in bytes; `None` is unlimited.
    pub artifact_quota_bytes: Option<u64>,
    /// Free space to keep on the artifact store's volume.
    pub min_free_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scheduler_enabled: true,
            max_parallel_attempts: 4,
            trusted_binaries: Vec::new(),
            model_access: true,
            operator_dirs: Vec::new(),
            allow_unsandboxed: false,
            approval_ttl_ms: 7 * 24 * 3_600_000,
            max_pending_approvals: 50,
            decisions_per_minute: 10,
            remote_approvals: false,
            artifact_quota_bytes: None,
            min_free_bytes: 512 << 20,
        }
    }
}

impl Config {
    /// Reads the file; the defaults when it does not exist.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Config`] for unreadable or invalid JSON.
    pub fn load(path: &Path) -> DaemonResult<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| DaemonError::Config(format!("config.json: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(DaemonError::Config(format!("config.json: {}", e.kind()))),
        }
    }
}
