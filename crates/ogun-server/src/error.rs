//! Errors of the bridge: always an O-00 envelope, returned as `{"error": <envelope>}` with the HTTP status of
//! its class (O-13 section 6.1).

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use ogun_api::ApiError;
use ogun_types::{ErrorClass, OgunError};

/// An error answered to an HTTP client.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{}: {}", envelope.code, envelope.message)]
pub struct ServerError {
    /// The O-00 envelope.
    pub envelope: OgunError,
    /// Seconds for `Retry-After`, for rate limits.
    pub retry_after_s: Option<u64>,
}

/// Result alias.
pub type ServerResult<T> = Result<T, ServerError>;

impl ServerError {
    /// An error for a registered code.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            envelope: OgunError::new(code, message),
            retry_after_s: None,
        }
    }

    /// `api.invalid_params`.
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("api.invalid_params", message)
    }

    /// `api.unauthenticated`.
    #[must_use]
    pub fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new("api.unauthenticated", message)
    }

    /// `api.permission_denied`.
    #[must_use]
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new("api.permission_denied", message)
    }

    /// `server.not_found`.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("server.not_found", message)
    }

    /// `api.unavailable`.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new("api.unavailable", message)
    }

    /// `internal.bug`.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal.bug", message)
    }

    /// `server.rate_limited` with a retry hint.
    #[must_use]
    pub fn rate_limited(retry_after_s: u64) -> Self {
        Self {
            envelope: OgunError::new("server.rate_limited", "too many requests; slow down"),
            retry_after_s: Some(retry_after_s.max(1)),
        }
    }

    /// `server.webauthn_invalid`.
    #[must_use]
    pub fn webauthn(message: impl Into<String>) -> Self {
        Self::new("server.webauthn_invalid", message)
    }

    /// The stable code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.envelope.code
    }

    /// The HTTP status for this error's class.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        status_for(self.envelope.class)
    }
}

/// HTTP status for an O-00 error class.
#[must_use]
pub fn status_for(class: ErrorClass) -> StatusCode {
    match class {
        ErrorClass::InvalidRequest | ErrorClass::Unsupported | ErrorClass::Permanent => {
            StatusCode::BAD_REQUEST
        }
        ErrorClass::Unauthenticated | ErrorClass::AuthExpired => StatusCode::UNAUTHORIZED,
        ErrorClass::PolicyDenied | ErrorClass::Blocked => StatusCode::FORBIDDEN,
        ErrorClass::NotFound | ErrorClass::UrlExpired => StatusCode::NOT_FOUND,
        ErrorClass::Conflict => StatusCode::CONFLICT,
        ErrorClass::Throttled => StatusCode::TOO_MANY_REQUESTS,
        ErrorClass::Unavailable | ErrorClass::TransientNetwork => StatusCode::SERVICE_UNAVAILABLE,
        ErrorClass::Timeout => StatusCode::GATEWAY_TIMEOUT,
        ErrorClass::Internal | ErrorClass::Disk | ErrorClass::Integrity => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

impl From<ApiError> for ServerError {
    fn from(e: ApiError) -> Self {
        Self {
            envelope: e.data,
            retry_after_s: None,
        }
    }
}

impl From<OgunError> for ServerError {
    fn from(envelope: OgunError) -> Self {
        Self {
            envelope,
            retry_after_s: None,
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = serde_json::json!({ "error": self.envelope });
        let mut resp = (status, axum::Json(body)).into_response();
        if let Some(s) = self.retry_after_s
            && let Ok(v) = HeaderValue::from_str(&s.to_string())
        {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_map_to_the_contract_statuses() {
        let cases = [
            ("api.invalid_params", 400),
            ("api.unauthenticated", 401),
            ("api.permission_denied", 403),
            ("workflow.not_found", 404),
            ("api.idempotency_conflict", 409),
            ("api.unavailable", 503),
            ("internal.bug", 500),
            ("server.rate_limited", 429),
        ];
        for (code, status) in cases {
            assert_eq!(
                ServerError::new(code, "x").status().as_u16(),
                status,
                "{code}"
            );
        }
    }
}
