//! Ogun error codes and the error envelope (O-00 section 5).
//!
//! The envelope shape and the classes are Zeus spec 00 section 9, unchanged. Ogun registers its own
//! namespaces (`ir`, `run`, `op`); a code, once published, keeps its meaning and its class forever.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zeus_types::ErrorClass;

/// Registry entry for one Ogun error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeInfo {
    /// The stable code, `<namespace>.<reason>`.
    pub code: &'static str,
    /// The one class this code always maps to.
    pub class: ErrorClass,
    /// Whether repeating the same request may succeed without human action.
    pub retryable: bool,
}

const fn code(code: &'static str, class: ErrorClass, retryable: bool) -> CodeInfo {
    CodeInfo {
        code,
        class,
        retryable,
    }
}

/// Every Ogun error code (O-00 section 5.2). Diagnostics are not errors and are not listed here.
pub const CODES: &[CodeInfo] = &[
    code("ir.invalid", ErrorClass::InvalidRequest, false),
    code("ir.too_large", ErrorClass::InvalidRequest, false),
    code("ir.version_unsupported", ErrorClass::Unsupported, false),
    code("ir.hash_mismatch", ErrorClass::Conflict, false),
    code("run.invalid_transition", ErrorClass::Conflict, false),
    code("run.not_found", ErrorClass::NotFound, false),
    code("run.approval_required", ErrorClass::PolicyDenied, false),
    code("run.budget_exhausted", ErrorClass::PolicyDenied, false),
    code("op.unknown", ErrorClass::Unsupported, false),
    code("op.sandbox_violation", ErrorClass::PolicyDenied, false),
    code("op.crashed", ErrorClass::Internal, false),
    code("op.timeout", ErrorClass::Timeout, true),
    code("internal.bug", ErrorClass::Internal, false),
    // The local API (O-15).
    code("api.unauthenticated", ErrorClass::Unauthenticated, false),
    code("api.permission_denied", ErrorClass::PolicyDenied, false),
    code("api.method_not_found", ErrorClass::Unsupported, false),
    code("api.invalid_params", ErrorClass::InvalidRequest, false),
    code("api.payload_too_large", ErrorClass::InvalidRequest, false),
    code("api.unavailable", ErrorClass::Unavailable, true),
    code("api.idempotency_conflict", ErrorClass::Conflict, false),
    code("workflow.not_found", ErrorClass::NotFound, false),
    code("workflow.not_approved", ErrorClass::PolicyDenied, false),
    // Approvals (O-07).
    code("approval.not_found", ErrorClass::NotFound, false),
    code("approval.hash_mismatch", ErrorClass::Conflict, false),
    code("approval.channel_required", ErrorClass::PolicyDenied, false),
    code("approval.already_decided", ErrorClass::Conflict, false),
    code("approval.expired", ErrorClass::PolicyDenied, false),
    code("artifact.not_found", ErrorClass::NotFound, false),
    code("stream.unknown", ErrorClass::NotFound, false),
    code("stream.full", ErrorClass::Unavailable, true),
    code("credential.not_found", ErrorClass::NotFound, false),
    code("credential.name_taken", ErrorClass::Conflict, false),
    code("secret.not_found", ErrorClass::NotFound, false),
    code("edge.not_enrolled", ErrorClass::PolicyDenied, false),
    code("sync.bad_signature", ErrorClass::PolicyDenied, false),
    code("sync.digest_mismatch", ErrorClass::Conflict, false),
    // Model operators and the provider ladder (O-03 section 8).
    code("llm.not_configured", ErrorClass::Unavailable, false),
    code("llm.config_invalid", ErrorClass::InvalidRequest, false),
    code("llm.provider_unavailable", ErrorClass::Unavailable, true),
    code("llm.auth_failed", ErrorClass::Unauthenticated, false),
    code("llm.request_rejected", ErrorClass::InvalidRequest, false),
    code("llm.refused", ErrorClass::PolicyDenied, false),
    code("llm.output_invalid", ErrorClass::Integrity, false),
    code("llm.call_limit", ErrorClass::PolicyDenied, false),
    code("llm.input_unsupported", ErrorClass::InvalidRequest, false),
];

/// Looks up a registered code.
#[must_use]
pub fn code_info(code: &str) -> Option<&'static CodeInfo> {
    CODES.iter().find(|info| info.code == code)
}

/// An Ogun error in the envelope shape of Zeus spec 00 section 9.1.
///
/// Messages are English diagnostics for people. They never contain secrets, URLs (use
/// `redact_url`) or values copied from a workflow's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
#[serde(deny_unknown_fields)]
pub struct OgunError {
    /// Stable machine identifier from [`CODES`].
    pub code: String,
    /// Coarse class; drives retry and presentation.
    pub class: ErrorClass,
    /// At most 1024 characters.
    pub message: String,
    /// True iff repeating the same request may succeed without human action.
    pub retryable: bool,
    /// Code-specific structured data, at most 8 KiB serialised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
}

impl OgunError {
    /// Builds an error for a registered code; class and `retryable` come from the registry. An
    /// unregistered code becomes `internal.bug`, so a typo cannot pass as a retryable condition.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 1024 {
            let mut end = 1024;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        match code_info(code) {
            Some(info) => Self {
                code: info.code.to_owned(),
                class: info.class,
                message,
                retryable: info.retryable,
                details: None,
            },
            None => Self {
                code: "internal.bug".to_owned(),
                class: ErrorClass::Internal,
                message: format!("unregistered error code {code}: {message}"),
                retryable: false,
                details: None,
            },
        }
    }

    /// Adds structured details.
    #[must_use]
    pub fn with_details(mut self, details: Map<String, Value>) -> Self {
        self.details = Some(details);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for info in CODES {
            assert!(seen.insert(info.code), "duplicate code {}", info.code);
            let (ns, reason) = info.code.split_once('.').expect("namespace.reason");
            assert!(
                [
                    "ir",
                    "run",
                    "op",
                    "internal",
                    "api",
                    "workflow",
                    "approval",
                    "artifact",
                    "stream",
                    "credential",
                    "secret",
                    "edge",
                    "sync",
                    "llm"
                ]
                .contains(&ns),
                "{}",
                info.code
            );
            assert!(reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        }
    }

    #[test]
    fn envelope_shape_matches_zeus_00() {
        let err = OgunError::new("run.invalid_transition", "node is succeeded");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["class"], "conflict");
        assert_eq!(json["retryable"], false);
        assert!(json.get("details").is_none());
    }

    #[test]
    fn unknown_code_is_an_internal_bug() {
        let err = OgunError::new("ir.typo", "x");
        assert_eq!(err.code, "internal.bug");
        assert_eq!(err.class, ErrorClass::Internal);
    }
}
