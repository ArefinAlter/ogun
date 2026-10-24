//! Gateway errors, reported to callers as the O-00 envelope.

use ogun_types::OgunError;

/// Why a gateway operation failed. Messages never carry payload bytes, secrets or URLs with credentials.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// The configuration is not acceptable.
    #[error("invalid gateway configuration: {0}")]
    Config(String),
    /// The ingest log is full under `block`: the adapter must stop acknowledging.
    #[error("the ingest log is full ({used} of {max} bytes)")]
    Full {
        /// Bytes in use.
        used: u64,
        /// The limit.
        max: u64,
    },
    /// The gateway has no `edg_` id yet: a person must enrol its key on the hub first (O-11 section 2).
    #[error("the gateway is not enrolled on a hub yet")]
    NotEnrolled,
    /// The gateway has not been opened.
    #[error("the gateway is not open")]
    NotOpen,
    /// A request named something the gateway does not hold.
    #[error("{0}")]
    NotFound(String),
    /// Storage failed (the ingest log, the outbox or a blob file).
    #[error("gateway storage: {0}")]
    Storage(String),
    /// Malformed input from the platform layer.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The OS has no random source.
    #[error("no OS random source")]
    Random,
}

impl GatewayError {
    /// The O-00 envelope for this error.
    #[must_use]
    pub fn envelope(&self) -> OgunError {
        let code = match self {
            Self::Config(_) | Self::Invalid(_) => "api.invalid_params",
            Self::Full { .. } => "stream.full",
            Self::NotEnrolled => "edge.not_enrolled",
            Self::NotOpen | Self::Random => "api.unavailable",
            Self::NotFound(_) => "artifact.not_found",
            Self::Storage(_) => "internal.bug",
        };
        OgunError::new(code, self.to_string())
    }
}

impl From<ogun_edge::LogError> for GatewayError {
    fn from(e: ogun_edge::LogError) -> Self {
        match e {
            ogun_edge::LogError::Full { used, max } => Self::Full { used, max },
            other => Self::Storage(other.to_string()),
        }
    }
}

impl From<std::io::Error> for GatewayError {
    fn from(e: std::io::Error) -> Self {
        Self::Storage(e.kind().to_string())
    }
}

/// Result alias.
pub type GatewayResult<T> = Result<T, GatewayError>;
