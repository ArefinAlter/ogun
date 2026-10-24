//! Errors of the model layer.
//!
//! Messages are English diagnostics for people. They never contain secrets, URLs with query strings, prompt or
//! document text, model output or values copied from a workflow's configuration. Provider names come from the
//! daemon's `llm.json`, not from a workflow, and may appear.

use ogun_types::OgunError;
use serde_json::{Map, Value};

/// A failure of the model layer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    /// No provider in the ladder can serve the request: none enabled, none with the needed capability, or
    /// every candidate lacks its API key.
    #[error("no model provider can serve this request: {0}")]
    NotConfigured(String),
    /// The ladder configuration or an operator's configuration is invalid.
    #[error("invalid model configuration at {at}: {reason}")]
    ConfigInvalid {
        /// Where, as a dotted path (`providers[1].base_url`, `config.labels`).
        at: String,
        /// What is wrong.
        reason: String,
    },
    /// HTTP 429 from the provider.
    #[error("provider {provider} is rate limiting requests")]
    Throttled {
        /// Provider name from the ladder.
        provider: String,
        /// The provider's `retry-after`, when it sent one.
        retry_after_ms: Option<u64>,
    },
    /// HTTP 5xx, a connection failure or an overloaded provider.
    #[error("provider {provider} is unavailable: {reason}")]
    Unavailable {
        /// Provider name from the ladder.
        provider: String,
        /// Short reason (`HTTP 503`, `connection failed`).
        reason: String,
    },
    /// The provider did not answer within its configured timeout.
    #[error("provider {provider} did not answer within {timeout_ms} ms")]
    Timeout {
        /// Provider name from the ladder.
        provider: String,
        /// The configured timeout.
        timeout_ms: u64,
    },
    /// HTTP 401 or 403: the API key was rejected.
    #[error("provider {provider} rejected the credential")]
    AuthFailed {
        /// Provider name from the ladder.
        provider: String,
    },
    /// The provider rejected the request as invalid (HTTP 400, 404, 413, 422 and similar).
    #[error("provider {provider} rejected the request ({reason})")]
    Rejected {
        /// Provider name from the ladder.
        provider: String,
        /// HTTP status and the provider's error type, never its message.
        reason: String,
    },
    /// The model or the provider declined on policy grounds (a refusal, a content filter, a safety block).
    #[error("provider {provider} declined the request on policy grounds ({reason})")]
    Refused {
        /// Provider name from the ladder.
        provider: String,
        /// The provider's stop reason or block category.
        reason: String,
    },
    /// The model's answer did not satisfy the requested schema, after the repair attempt.
    #[error("model output did not satisfy the requested schema: {reason}")]
    OutputInvalid {
        /// Instance paths and failed keywords; never the output itself.
        reason: String,
    },
    /// A call's worst-case cost exceeds the remaining budget.
    #[error(
        "worst-case cost {needed_usd_micro} micro-USD exceeds the remaining budget of {remaining_usd_micro} micro-USD"
    )]
    BudgetExceeded {
        /// Worst-case cost of the refused call.
        needed_usd_micro: u64,
        /// What was left.
        remaining_usd_micro: u64,
    },
    /// The operator's `max_calls` would be exceeded.
    #[error("the operator reached its limit of {max_calls} model calls")]
    CallLimit {
        /// The configured limit.
        max_calls: u64,
    },
    /// An input artifact cannot be used (media type, size, encoding, shape).
    #[error("unsupported input: {0}")]
    Input(String),
    /// The spend ledger cannot be read, verified or written.
    #[error("spend ledger: {0}")]
    Ledger(String),
    /// An invariant was violated.
    #[error("internal: {0}")]
    Internal(String),
}

impl LlmError {
    /// Shorthand for [`LlmError::ConfigInvalid`].
    #[must_use]
    pub fn config(at: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ConfigInvalid {
            at: at.into(),
            reason: reason.into(),
        }
    }

    /// Whether repeating the same request later may succeed without human action.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Throttled { .. } | Self::Unavailable { .. } | Self::Timeout { .. }
        )
    }

    /// Whether the ladder may try the next provider after this failure. Only transient provider failures fall
    /// back; policy, credential and invalid-request failures never do.
    #[must_use]
    pub fn falls_back(&self) -> bool {
        self.is_retryable()
    }

    /// The registered error code (O-00 section 5.2).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured(_) => "llm.not_configured",
            Self::ConfigInvalid { .. } => "llm.config_invalid",
            Self::Throttled { .. } | Self::Unavailable { .. } | Self::Timeout { .. } => {
                "llm.provider_unavailable"
            }
            Self::AuthFailed { .. } => "llm.auth_failed",
            Self::Rejected { .. } => "llm.request_rejected",
            Self::Refused { .. } => "llm.refused",
            Self::OutputInvalid { .. } => "llm.output_invalid",
            Self::BudgetExceeded { .. } => "run.budget_exhausted",
            Self::CallLimit { .. } => "llm.call_limit",
            Self::Input(_) => "llm.input_unsupported",
            Self::Ledger(_) | Self::Internal(_) => "internal.bug",
        }
    }

    /// The error envelope (Zeus 00 section 9.1) with structured details.
    #[must_use]
    pub fn to_ogun_error(&self) -> OgunError {
        let mut details = Map::new();
        match self {
            Self::ConfigInvalid { at, .. } => {
                details.insert("at".into(), Value::from(at.as_str()));
            }
            Self::Throttled {
                provider,
                retry_after_ms,
            } => {
                details.insert("provider".into(), Value::from(provider.as_str()));
                if let Some(ms) = retry_after_ms {
                    details.insert("retry_after_ms".into(), Value::from(*ms));
                }
            }
            Self::Unavailable { provider, .. }
            | Self::Timeout { provider, .. }
            | Self::AuthFailed { provider }
            | Self::Rejected { provider, .. }
            | Self::Refused { provider, .. } => {
                details.insert("provider".into(), Value::from(provider.as_str()));
            }
            Self::BudgetExceeded {
                needed_usd_micro,
                remaining_usd_micro,
            } => {
                details.insert("needed_usd_micro".into(), Value::from(*needed_usd_micro));
                details.insert(
                    "remaining_usd_micro".into(),
                    Value::from(*remaining_usd_micro),
                );
            }
            Self::CallLimit { max_calls } => {
                details.insert("max_calls".into(), Value::from(*max_calls));
            }
            _ => {}
        }
        let err = OgunError::new(self.code(), self.to_string());
        if details.is_empty() {
            err
        } else {
            err.with_details(details)
        }
    }
}

impl From<LlmError> for OgunError {
    fn from(e: LlmError) -> Self {
        e.to_ogun_error()
    }
}
