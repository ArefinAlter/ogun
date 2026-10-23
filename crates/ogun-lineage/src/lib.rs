//! Ogun lineage and run records (O-05).
//!
//! Three things live here:
//!
//! * [`openlineage`]: OpenLineage 2.x `RunEvent` documents built from what the run store already holds
//!   (`ogun_store::RunRecord`, `NodeInstance` and `Event`) plus a plain description of the artifacts each node
//!   instance committed ([`OutputArtifact`]). The crate never talks to `ogund`; the daemon, the CLI or Studio read
//!   the store and the artifact index and hand the rows in.
//! * [`export`]: writing those events as JSON Lines, and posting them to an OpenLineage HTTP endpoint (Marquez)
//!   that a person configured. Endpoint URLs reach errors and logs only through `redact_url`.
//! * [`records`]: the shapes the benchmark profile (O-06) records: agent runs (`agr_`), their append-only
//!   trajectories with a BLAKE3 chain, and cost-ledger entries.
//!
//! Nothing in this crate decides anything: it describes runs that happened. Values copied from workflow
//! configuration never appear in a facet; only identifiers, digests, sizes, media types, states and causes do.

#![forbid(unsafe_code)]

pub mod export;
pub mod openlineage;
pub mod records;

pub use export::{HttpEndpoint, read_jsonl, to_jsonl, write_jsonl};
pub use openlineage::{
    EventType, LineageConfig, OperatorInfo, OutputArtifact, PartitionWindow, RunEvent, RunLineage,
    uuid_from_ulid,
};
pub use records::{
    AgentInfo, AgentRunOutcome, AgentRunRecord, CostKind, CostRecord, CostTotals, Score, TaskRef,
    TrajectoryEvent, TrajectoryKind, TrajectoryRef, TrajectorySummary, TrajectoryWriter,
    verify_trajectory,
};

/// Lineage failures. Messages never contain URLs other than redacted ones, secrets or configuration values.
#[derive(Debug, thiserror::Error)]
pub enum LineageError {
    /// The rows handed in disagree with each other (an event for an unknown instance, an output of another run).
    #[error("lineage input is inconsistent: {0}")]
    Inconsistent(String),
    /// Reading or writing a file failed.
    #[error("lineage file I/O failed: {0}")]
    Io(std::io::ErrorKind),
    /// A document could not be encoded or decoded.
    #[error("lineage JSON is invalid: {0}")]
    Json(String),
    /// The endpoint URL is not one this build can post to.
    #[error("lineage endpoint {endpoint} is not supported: {reason}")]
    EndpointUnsupported {
        /// The endpoint, redacted.
        endpoint: String,
        /// Why.
        reason: &'static str,
    },
    /// The endpoint could not be reached or did not answer in time.
    #[error("lineage endpoint {endpoint} unreachable: {reason}")]
    EndpointUnreachable {
        /// The endpoint, redacted.
        endpoint: String,
        /// What failed (an I/O error kind or "timeout"), never response text.
        reason: String,
    },
    /// The endpoint answered with a status other than 2xx.
    #[error("lineage endpoint {endpoint} refused the event with HTTP status {status}")]
    EndpointRejected {
        /// The endpoint, redacted.
        endpoint: String,
        /// HTTP status.
        status: u16,
    },
    /// A trajectory file is not an unbroken chain.
    #[error("trajectory is broken at sequence number {seq}: {reason}")]
    TrajectoryBroken {
        /// The first bad line's sequence number (or line number when it has none).
        seq: u64,
        /// What is wrong.
        reason: &'static str,
    },
}

impl LineageError {
    /// The O-00 error code for this failure.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Inconsistent(_) | Self::Json(_) => "internal.bug",
            Self::Io(_) => "lineage.io_failed",
            Self::EndpointUnsupported { .. } => "lineage.endpoint_unsupported",
            Self::EndpointUnreachable { .. } | Self::EndpointRejected { .. } => {
                "lineage.endpoint_failed"
            }
            Self::TrajectoryBroken { .. } => "lineage.trajectory_broken",
        }
    }

    /// The error in the O-00 envelope.
    #[must_use]
    pub fn to_ogun_error(&self) -> ogun_types::OgunError {
        ogun_types::OgunError::new(self.code(), self.to_string())
    }
}

impl From<std::io::Error> for LineageError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.kind())
    }
}

impl From<serde_json::Error> for LineageError {
    fn from(e: serde_json::Error) -> Self {
        // serde_json messages can quote input; keep only the category and position.
        Self::Json(format!("{:?} at line {}", e.classify(), e.line()))
    }
}

/// Result alias.
pub type LineageResult<T> = Result<T, LineageError>;
