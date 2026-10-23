//! The Zeus session bridge (O-08).
//!
//! Ogun never holds browser session material (constraint 2). When a workflow needs a source behind the user's
//! browser session, `ogund` asks `zeusd` to run that download as a Zeus job, over the Zeus daemon API (Zeus spec
//! 05), as a `service` client: a person links the two products once with the Zeus CLI
//! (`zeus clients add --kind service --name ogun`), which writes a service credential file; Ogun's configuration
//! names that file ([`BridgeConfig::credential_file`]).
//!
//! This crate is Ogun's own spec 05 client (ADR 0006: no dependency on `zeus-api`): the endpoint, the line
//! framing, the mutual handshake of section 6 and the handful of methods the bridge needs. On top of it,
//! [`ZeusBridge`] offers what the daemon uses:
//!
//! * [`ZeusBridge::connect`]: handshake, refusing any credential that is not of kind `service` (Ogun must never
//!   hold human-capable Zeus authority);
//! * [`ZeusBridge::register_root`]: `files.roots.register` for Ogun's import directory;
//! * [`ZeusBridge::submit`]: one `jobs.enqueue` from the request an operator returned as
//!   `Outcome::SessionRequired` (O-08 section 6), with the destination forced into the registered root;
//! * [`ZeusBridge::status`] and [`ZeusBridge::wait`]: the job as running, waiting for a Zeus approval, waiting
//!   for a session, succeeded with file paths and provenance, failed or cancelled;
//! * [`ZeusBridge::import`]: a completed job's file and provenance as an import into Ogun's own `zeus-artifact`
//!   store.
//!
//! The bridge can call only the methods in [`client::METHODS`]. `approvals.decide`, `sessions.revoke` and every
//! other human-only method are not among them: session-release approvals are Zeus's, decided by a person on a
//! Zeus trust surface, and Ogun only observes that a job waits for one.

#![forbid(unsafe_code)]

pub mod bridge;
pub mod client;
pub mod config;
pub mod endpoint;
pub mod job;
pub mod proof;

pub use bridge::{ImportedArtifact, PendingImport, SubmitOptions, Submitted, ZeusBridge};
pub use client::ZeusClient;
pub use config::BridgeConfig;
pub use job::{CompletedJob, JobStatus, ZeusFile, ZeusProvenance};

use zeus_types::ErrorEnvelope;

/// Bridge failures. Messages never contain URLs other than redacted ones, tokens or proofs.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// No usable service credential: the file is missing or malformed, or Zeus refused it.
    #[error("Ogun is not linked to Zeus: {0}")]
    NotLinked(String),
    /// `zeusd` is not running or the connection broke.
    #[error("zeusd is not reachable: {0}")]
    Unavailable(String),
    /// The endpoint answered but could not prove it holds this user's `ipc.secret`; nothing was sent.
    #[error("the endpoint did not prove it is this user's zeusd")]
    ServerUnverified,
    /// The credential is not a `service` credential.
    #[error("the Zeus credential has kind {0}; the bridge only uses a service credential")]
    WrongKind(String),
    /// `zeusd` answered with an error envelope.
    #[error("zeusd answered {}: {}", .0.code, .0.message)]
    Zeus(Box<ErrorEnvelope>),
    /// The daemon's answer did not have the expected shape.
    #[error("unexpected answer from zeusd: {0}")]
    Protocol(String),
    /// A session request from an operator is not acceptable.
    #[error("invalid session request: {0}")]
    InvalidRequest(String),
    /// A path is outside the registered import roots.
    #[error("path is outside Ogun's registered Zeus roots")]
    PathOutsideRoot,
    /// The job has not completed, so there is nothing to import.
    #[error("the Zeus job is not completed")]
    NotCompleted,
    /// Ogun's artifact store refused the import.
    #[error("the artifact store refused the import: {0}")]
    Artifact(String),
    /// Local file-system access failed.
    #[error("file access failed: {0:?}")]
    Io(std::io::ErrorKind),
}

impl BridgeError {
    /// The O-00 error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotLinked(_) => "zeus.not_linked",
            Self::Unavailable(_) => "zeus.unavailable",
            Self::ServerUnverified => "zeus.server_unverified",
            Self::WrongKind(_) => "zeus.wrong_kind",
            Self::Zeus(e) if e.retryable => "zeus.call_failed",
            Self::Zeus(_) => "zeus.refused",
            Self::Protocol(_) => "zeus.protocol_error",
            Self::InvalidRequest(_) => "zeus.request_invalid",
            Self::PathOutsideRoot => "zeus.path_outside_root",
            Self::NotCompleted => "zeus.job_not_completed",
            Self::Artifact(_) => "zeus.import_failed",
            Self::Io(_) => "zeus.unavailable",
        }
    }

    /// The error in Ogun's envelope; Zeus's own code and class go in `details`.
    #[must_use]
    pub fn to_ogun_error(&self) -> ogun_types::OgunError {
        let e = ogun_types::OgunError::new(self.code(), self.to_string());
        if let Self::Zeus(z) = self {
            let mut d = serde_json::Map::new();
            d.insert(
                "zeus_code".into(),
                serde_json::Value::String(z.code.to_string()),
            );
            if let Ok(class) = serde_json::to_value(z.class) {
                d.insert("zeus_class".into(), class);
            }
            return e.with_details(d);
        }
        e
    }

    /// Whether the call may succeed if repeated later without a person acting.
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unavailable(_) | Self::Io(_) => true,
            Self::Zeus(e) => e.retryable,
            _ => false,
        }
    }
}

impl From<std::io::Error> for BridgeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.kind())
    }
}

/// Result alias.
pub type BridgeResult<T> = Result<T, BridgeError>;
