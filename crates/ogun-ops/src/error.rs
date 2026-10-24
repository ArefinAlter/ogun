//! Failures inside the operator runtime and their mapping to [`Outcome::Failed`](crate::Outcome).
//!
//! Every failure carries a registered code (O-00 section 5.2) and an English message that never quotes
//! a configuration value, a secret or an unredacted URL. `retryable` is the registry's flag for the code,
//! so the class always decides between `retryable_error` and `permanent_error` (O-03 section 5).

use ogun_types::OgunError;

use crate::Outcome;

/// An operator failure: the envelope and whether the daemon may retry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{error}")]
pub struct OpError {
    /// The envelope.
    pub error: OgunError,
    /// Retry if retries remain.
    pub retryable: bool,
}

impl OpError {
    /// A failure with a registered code; `retryable` comes from the registry.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        let error = OgunError::new(code, message);
        let retryable = error.retryable;
        Self { error, retryable }
    }

    /// Adds structured details (never secrets or configuration values).
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Map<String, serde_json::Value>) -> Self {
        self.error = self.error.with_details(details);
        self
    }

    /// The outcome for this failure.
    #[must_use]
    pub fn into_outcome(self) -> Outcome {
        Outcome::Failed {
            error: self.error,
            retryable: self.retryable,
        }
    }
}

impl From<OgunError> for OpError {
    fn from(error: OgunError) -> Self {
        let retryable = error.retryable;
        Self { error, retryable }
    }
}

/// Result type of the operator runtime.
pub type OpResult<T> = Result<T, OpError>;

/// `op.config_invalid`.
pub(crate) fn config(message: impl Into<String>) -> OpError {
    OpError::new("op.config_invalid", message)
}

/// `op.input_invalid`.
pub(crate) fn input(message: impl Into<String>) -> OpError {
    OpError::new("op.input_invalid", message)
}

/// `op.io` for a local file operation; the OS error kind is kept, the path is not.
pub(crate) fn io(what: &str, err: &std::io::Error) -> OpError {
    OpError::new("op.io", format!("{what}: {}", err.kind()))
}

/// `op.limit_exceeded`.
pub(crate) fn limit(message: impl Into<String>) -> OpError {
    OpError::new("op.limit_exceeded", message)
}

/// `op.unsupported`.
pub(crate) fn unsupported(message: impl Into<String>) -> OpError {
    OpError::new("op.unsupported", message)
}

/// `internal.bug`.
pub(crate) fn internal(message: impl Into<String>) -> OpError {
    OpError::new("internal.bug", message)
}
