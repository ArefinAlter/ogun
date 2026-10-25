//! Studio errors: always the O-00 envelope, so the frontend handles daemon, hub and shell errors alike.

use ogun_api::ApiError;
use ogun_types::OgunError;
use serde::Serialize;

/// An error returned to the webview. It never carries a token, a URL query string or a payload.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[serde(transparent)]
#[error("{}: {}", .0.code, .0.message)]
pub struct StudioError(pub OgunError);

impl StudioError {
    /// An error for a registered Ogun code.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self(OgunError::new(code, message))
    }

    /// `api.invalid_params`.
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("api.invalid_params", message)
    }

    /// `api.permission_denied`.
    #[must_use]
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new("api.permission_denied", message)
    }

    /// `api.unavailable`.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new("api.unavailable", message)
    }

    /// The stable code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.0.code
    }
}

impl From<ApiError> for StudioError {
    fn from(e: ApiError) -> Self {
        Self(e.data)
    }
}

impl From<OgunError> for StudioError {
    fn from(e: OgunError) -> Self {
        Self(e)
    }
}

/// Result alias.
pub type StudioResult<T> = Result<T, StudioError>;
