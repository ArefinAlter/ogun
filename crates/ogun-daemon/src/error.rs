//! Daemon errors. Handlers return [`ogun_api::ApiError`]; this type covers startup.

use ogun_api::ApiError;
use ogun_store::StoreError;

/// Why `ogund` could not start or keep running.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// Bad configuration.
    #[error("configuration: {0}")]
    Config(String),
    /// Filesystem setup failed.
    #[error("state directory: {0}")]
    State(String),
    /// The run store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The artifact store failed.
    #[error("artifact store: {0}")]
    Artifacts(String),
    /// The API layer failed (binding the endpoint, for example).
    #[error(transparent)]
    Api(#[from] ApiError),
}

/// Result alias.
pub type DaemonResult<T> = Result<T, DaemonError>;

/// Maps a store error to an API error without leaking storage details.
pub fn store_err(e: StoreError) -> ApiError {
    match &e {
        StoreError::NotFound(_) => ApiError::new("run.not_found", "not found"),
        StoreError::InvalidTransition(t) => ApiError::new("run.invalid_transition", t.to_string()),
        StoreError::AlreadyOpen => {
            ApiError::unavailable("the run store is held by another process")
        }
        _ => {
            tracing::error!(error = %e, "run store failure");
            ApiError::internal("run store failure")
        }
    }
}

/// Maps an artifact store error.
pub fn artifact_err(e: &zeus_artifact::ArtifactError) -> ApiError {
    match e {
        zeus_artifact::ArtifactError::NotFound | zeus_artifact::ArtifactError::ObjectAbsent => {
            ApiError::new("artifact.not_found", "no such artifact")
        }
        zeus_artifact::ArtifactError::QuotaExceeded => {
            ApiError::new("run.budget_exhausted", "the artifact store is full")
        }
        other => {
            tracing::error!(error = %other, "artifact store failure");
            ApiError::internal("artifact store failure")
        }
    }
}
