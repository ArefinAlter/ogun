//! Ogun identifiers and error codes (Ogun spec O-00).
//!
//! Ogun adopts Zeus spec 00 by reference: prefixed ULIDs, the error envelope and its classes,
//! RFC 3339 instants with milliseconds, `_ms` durations and redaction. This crate adds only what is
//! Ogun's own: the identifier prefixes of O-00 section 3 and the error codes of O-00 section 5.
//! Everything else is re-exported from `zeus-types` so both products share one vocabulary.

#![forbid(unsafe_code)]

pub mod error;
pub mod ids;

pub use error::{CodeInfo, OgunError, code_info};
pub use ids::{
    AgentRunId, ApprovalId, AssetId, EdgeId, EnvironmentId, EvaluatorId, IdError, IdGenerator,
    NodeInstanceId, RunId, SyncSessionId, WorkflowId,
};
pub use zeus_types::{ErrorClass, Timestamp, redact_url};
