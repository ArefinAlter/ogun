//! HTTPS clients for the provider kinds (feature `http`).
//!
//! No redirects are followed (a key never travels to a host the user did not configure), no hidden retries
//! are made (the ladder decides about fallback), and errors carry the provider name, the HTTP status and the
//! provider's error type, never response text, URLs or keys.

mod anthropic;
mod google;
mod ollama;
mod openai;

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use zeus_types::{RedactMode, redact_url};

use crate::budget::{cost_usd_micro, estimate_tokens, input_token_bound};
use crate::config::{Capability, ProviderConfig, ProviderKind, is_loopback};
use crate::error::LlmError;
use crate::request::{ChatRequest, ChatResponse, EmbedRequest, EmbedResponse, Usage};
use crate::schema;
use crate::secrets::ApiKey;

/// Largest chat response body read.
const CHAT_BODY_LIMIT: usize = 16 * 1024 * 1024;
/// Largest embeddings response body read.
const EMBED_BODY_LIMIT: usize = 128 * 1024 * 1024;
/// Extra prompt bytes assumed for instruction text and framing when bounding a call's cost.
const PROMPT_SLACK_BYTES: u64 = 1024;

/// How the key is sent.
#[derive(Debug, Clone, Copy)]
enum Auth {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// A named header carrying the key.
    Header(&'static str),
}

/// One configured provider with its key and HTTP client.
pub struct Provider {
    cfg: ProviderConfig,
    key: Option<ApiKey>,
    base: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("name", &self.cfg.name)
            .field("kind", &self.cfg.kind)
            .field("model", &self.cfg.model)
            .field("base", &redact_url(&self.base, RedactMode::Log).url)
            .finish_non_exhaustive()
    }
}

impl Provider {
    /// A provider from a validated entry. Performs no network request.
    pub fn new(cfg: ProviderConfig, key: Option<ApiKey>) -> Result<Self, LlmError> {
        if !cfg.enabled {
            return Err(LlmError::NotConfigured(format!(
                "provider {} is disabled",
                cfg.name
            )));
        }
        let key = key.filter(|k| !k.is_empty());
        if cfg.kind.requires_key() && key.is_none() {
            return Err(LlmError::NotConfigured(format!(
                "provider {} has no API key",
                cfg.name
            )));
        }
        let base = cfg.base()?;
        let timeout = Duration::from_millis(cfg.timeout_ms);
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .user_agent(concat!("ogun-op-llm/", env!("CARGO_PKG_VERSION")));
        if is_loopback(&base) {
            builder = builder.no_proxy();
        }
        let http = builder
            .build()
            .map_err(|_| LlmError::Internal("cannot configure the HTTP client".into()))?;
        Ok(Self {
            base: base.as_str().trim_end_matches('/').to_owned(),
            cfg,
            key,
            http,
        })
    }

    /// The provider's name in the ladder.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.cfg.name
    }

    /// Its configuration.
    #[must_use]
    pub fn config(&self) -> &ProviderConfig {
        &self.cfg
    }

    /// Whether answers are constrained natively for a request with a schema.
    #[must_use]
    pub fn native_schema(&self) -> bool {
        self.cfg.has(Capability::JsonSchema)
    }

    /// The output limit for a call.
    #[must_use]
    pub fn max_output(&self, requested: Option<u32>) -> u32 {
        requested
            .map_or(self.cfg.max_output_tokens, |r| {
                r.min(self.cfg.max_output_tokens)
            })
            .max(1)
    }

    /// The worst-case cost of a chat call.
    #[must_use]
    pub fn worst_case_chat(&self, req: &ChatRequest) -> u64 {
        cost_usd_micro(
            input_token_bound(req.prompt_bytes().saturating_add(PROMPT_SLACK_BYTES)),
            u64::from(self.max_output(req.max_output_tokens)),
            self.cfg.input_per_mtok_usd_micro,
            self.cfg.output_per_mtok_usd_micro,
        )
    }

    /// The worst-case cost of an embeddings call.
    #[must_use]
    pub fn worst_case_embed(&self, req: &EmbedRequest) -> u64 {
        let framing = 16 * req.inputs.len() as u64;
        cost_usd_micro(
            input_token_bound(req.input_bytes().saturating_add(framing)),
            0,
            self.cfg.input_per_mtok_usd_micro,
            0,
        )
    }

    /// One chat call. With a schema and no native mode, the schema instruction is appended to the system
    /// prompt; the answer is validated by the caller either way.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let mut req = req.clone();
        let native = req.schema.is_some() && self.native_schema();
        if !native && let Some(s) = req.schema.take() {
            req.system = format!("{}\n\n{}", req.system, schema::instruction(&s));
        }
        match self.cfg.kind {
            ProviderKind::Anthropic => anthropic::chat(self, &req).await,
            ProviderKind::Openai | ProviderKind::OpenaiCompatible => openai::chat(self, &req).await,
            ProviderKind::Ollama => ollama::chat(self, &req).await,
            ProviderKind::Google => google::chat(self, &req).await,
        }
    }

    /// One embeddings call.
    pub async fn embed(&self, req: &EmbedRequest) -> Result<EmbedResponse, LlmError> {
        if !self.cfg.has(Capability::Embeddings) {
            return Err(LlmError::NotConfigured(format!(
                "provider {} does not offer embeddings",
                self.cfg.name
            )));
        }
        let out = match self.cfg.kind {
            ProviderKind::Openai | ProviderKind::OpenaiCompatible => openai::embed(self, req).await,
            ProviderKind::Ollama => ollama::embed(self, req).await,
            ProviderKind::Google => google::embed(self, req).await,
            ProviderKind::Anthropic => Err(LlmError::NotConfigured(
                "anthropic providers do not offer embeddings".into(),
            )),
        }?;
        if out.vectors.len() != req.inputs.len() {
            return Err(LlmError::Unavailable {
                provider: self.cfg.name.clone(),
                reason: "the response has the wrong number of vectors".into(),
            });
        }
        Ok(out)
    }

    /// Usage of a call; counts the provider did not report are estimated from bytes.
    fn usage(
        &self,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        input_bytes: u64,
        output_bytes: u64,
    ) -> Usage {
        let input = input_tokens.unwrap_or_else(|| estimate_tokens(input_bytes));
        let output = output_tokens.unwrap_or_else(|| estimate_tokens(output_bytes));
        Usage {
            provider: self.cfg.name.clone(),
            model: self.cfg.model.clone(),
            input_tokens: input,
            output_tokens: output,
            cost_usd_micro: cost_usd_micro(
                input,
                output,
                self.cfg.input_per_mtok_usd_micro,
                self.cfg.output_per_mtok_usd_micro,
            ),
        }
    }

    fn malformed(&self, what: &str) -> LlmError {
        LlmError::Unavailable {
            provider: self.cfg.name.clone(),
            reason: format!("malformed response: {what}"),
        }
    }

    fn refused(&self, reason: &str) -> LlmError {
        LlmError::Refused {
            provider: self.cfg.name.clone(),
            reason: ident(reason).unwrap_or("policy").to_owned(),
        }
    }

    /// POSTs JSON and returns the parsed success body, or the classified failure.
    async fn post(
        &self,
        path: &str,
        body: &Value,
        auth: Auth,
        extra: &[(&'static str, &str)],
        limit: usize,
    ) -> Result<Value, LlmError> {
        let url = format!("{}{path}", self.base);
        let mut headers = HeaderMap::new();
        if let Some(key) = &self.key {
            let (name, value) = match auth {
                Auth::Bearer => (
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", key.expose()),
                ),
                Auth::Header(name) => (HeaderName::from_static(name), key.expose().to_owned()),
            };
            let value = zeroize::Zeroizing::new(value);
            let mut hv = HeaderValue::from_str(&value).map_err(|_| LlmError::AuthFailed {
                provider: self.cfg.name.clone(),
            })?;
            hv.set_sensitive(true);
            headers.insert(name, hv);
        }
        for (name, value) in extra {
            let hv = HeaderValue::from_str(value)
                .map_err(|_| LlmError::config("api_version", "not a valid header value"))?;
            headers.insert(HeaderName::from_static(name), hv);
        }
        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(|e| self.transport_error(&e))?;
        let status = response.status().as_u16();
        let retry_after_ms = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|s| s.saturating_mul(1000));
        let bytes = read_limited(response, limit).await.map_err(|e| match e {
            ReadError::TooLarge => LlmError::Rejected {
                provider: self.cfg.name.clone(),
                reason: "response too large".into(),
            },
            ReadError::Transport(e) => self.transport_error(&e),
        })?;
        if (200..300).contains(&status) {
            return serde_json::from_slice(&bytes).map_err(|_| self.malformed("not JSON"));
        }
        Err(self.status_error(status, retry_after_ms, &bytes))
    }

    fn transport_error(&self, e: &reqwest::Error) -> LlmError {
        // reqwest's own message includes the URL; it is never used.
        if e.is_timeout() {
            LlmError::Timeout {
                provider: self.cfg.name.clone(),
                timeout_ms: self.cfg.timeout_ms,
            }
        } else {
            LlmError::Unavailable {
                provider: self.cfg.name.clone(),
                reason: if e.is_connect() {
                    "connection failed".into()
                } else {
                    "request failed".into()
                },
            }
        }
    }

    fn status_error(&self, status: u16, retry_after_ms: Option<u64>, body: &[u8]) -> LlmError {
        let provider = self.cfg.name.clone();
        let kind = error_ident(body);
        let reason = match &kind {
            Some(k) => format!("HTTP {status} {k}"),
            None => format!("HTTP {status}"),
        };
        let policy = kind.as_deref().is_some_and(|k| {
            let k = k.to_ascii_lowercase();
            k.contains("content_policy") || k.contains("content_filter") || k.contains("safety")
        });
        match status {
            429 => LlmError::Throttled {
                provider,
                retry_after_ms,
            },
            401 | 403 => LlmError::AuthFailed { provider },
            408 => LlmError::Timeout {
                provider,
                timeout_ms: self.cfg.timeout_ms,
            },
            500..=599 => LlmError::Unavailable { provider, reason },
            _ if policy => LlmError::Refused {
                provider,
                reason: kind.unwrap_or_default(),
            },
            _ => LlmError::Rejected { provider, reason },
        }
    }
}

enum ReadError {
    TooLarge,
    Transport(reqwest::Error),
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, ReadError> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(ReadError::TooLarge);
    }
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ReadError::Transport)? {
        if out.len() + chunk.len() > limit {
            return Err(ReadError::TooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// A short machine identifier, or `None` for anything that could be free text.
fn ident(s: &str) -> Option<&str> {
    (!s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')))
    .then_some(s)
}

/// The provider's error type from an error body (`error.type`, `error.code`, `error.status`), never its
/// message.
fn error_ident(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error")?;
    for key in ["type", "code", "status"] {
        if let Some(s) = err.get(key).and_then(Value::as_str)
            && let Some(id) = ident(s)
        {
            return Some(id.to_owned());
        }
    }
    None
}

/// `u64` from a JSON member, when present.
fn count(v: &Value) -> Option<u64> {
    v.as_u64()
}

/// A JSON array of numbers as an `f32` vector.
fn vector(v: &Value) -> Option<Vec<f32>> {
    v.as_array()?
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32))
        .collect()
}

/// Temperature for the wire.
fn temperature(milli: u32) -> Value {
    Value::from(f64::from(milli) / 1000.0)
}
