//! API errors: the O-00 envelope plus the JSON-RPC error code that carries it.

use ogun_types::OgunError;
use serde::{Deserialize, Serialize};

/// An error crossing the local API. `data` is always an O-00 envelope; `message` repeats its message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{}: {}", data.code, data.message)]
pub struct ApiError {
    /// JSON-RPC error code (O-15 section 6): -32600 to -32603 for protocol errors, -32000 otherwise.
    pub code: i64,
    /// The message, for clients that do not read `data`.
    pub message: String,
    /// The O-00 envelope.
    pub data: OgunError,
}

/// Result alias.
pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    /// An error for a registered Ogun code.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        let data = OgunError::new(code, message);
        let rpc = match data.code.as_str() {
            "api.method_not_found" => -32601,
            "api.invalid_params" => -32602,
            "internal.bug" => -32603,
            _ => -32000,
        };
        Self {
            code: rpc,
            message: data.message.clone(),
            data,
        }
    }

    /// `api.invalid_params`.
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new("api.invalid_params", message)
    }

    /// `internal.bug`.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal.bug", message)
    }

    /// `api.unavailable`.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new("api.unavailable", message)
    }

    /// `api.permission_denied`.
    #[must_use]
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new("api.permission_denied", message)
    }

    /// The stable code, `<namespace>.<reason>`.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.data.code
    }
}

impl From<OgunError> for ApiError {
    fn from(data: OgunError) -> Self {
        Self {
            code: -32000,
            message: data.message.clone(),
            data,
        }
    }
}
