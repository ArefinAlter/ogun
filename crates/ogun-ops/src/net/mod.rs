//! Network building blocks for `crawl` and `acquire`: allowed domains, robots.txt, the scoped transport
//! and document fetching. Everything goes through the zeus-core HTTP stack; nothing here ever handles
//! cookies or browser session material.

pub mod domains;
pub mod fetch;
pub mod robots;
pub mod transport;

use std::time::Duration;

use http::header::HeaderName;
use serde_json::{Map, Value};
use zeroize::Zeroizing;

use crate::error::{OpResult, config};
use crate::secrets::ResolvedSecrets;
use transport::Injection;

/// Headers a configuration may never set: the transport, the engine or the session bridge own them.
const FORBIDDEN_HEADERS: &[&str] = &[
    "cookie",
    "set-cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "accept-encoding",
    "range",
    "if-range",
    "referer",
    "origin",
    "user-agent",
];

/// Reads `headers` and `query_secrets` from a configuration, resolving secret references.
///
/// # Errors
///
/// `op.config_invalid` for a forbidden or malformed header, or an unresolvable value.
pub fn injection(cfg: &Map<String, Value>, secrets: &ResolvedSecrets) -> OpResult<Injection> {
    let mut out = Injection::default();
    if let Some(v) = cfg.get("headers") {
        let map = v
            .as_object()
            .ok_or_else(|| config("headers must be an object"))?;
        if map.len() > 16 {
            return Err(config("at most 16 headers"));
        }
        for (name, value) in map {
            let lower = name.to_ascii_lowercase();
            if FORBIDDEN_HEADERS.contains(&lower.as_str())
                || lower.starts_with("proxy-")
                || lower.starts_with("sec-")
            {
                return Err(config(format!(
                    "the header {lower} may not be set by configuration"
                )));
            }
            let header = HeaderName::from_bytes(lower.as_bytes())
                .map_err(|_| config("a header name is not valid"))?;
            let text: Zeroizing<String> = secrets.text_of(value).ok_or_else(|| {
                config(format!(
                    "the header {lower} must be a string or a secret_ref"
                ))
            })?;
            if text.len() > 8192 || text.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
                return Err(config(format!(
                    "the value of header {lower} is too long or has control characters"
                )));
            }
            out.headers.push((header, text));
        }
    }
    if let Some(v) = cfg.get("query_secrets") {
        let map = v
            .as_object()
            .ok_or_else(|| config("query_secrets must be an object"))?;
        if map.len() > 16 {
            return Err(config("at most 16 query_secrets"));
        }
        for (name, value) in map {
            if name.is_empty() || name.len() > 128 {
                return Err(config("a query_secrets name is empty or too long"));
            }
            let text = secrets
                .text_of(value)
                .ok_or_else(|| config(format!("query_secrets.{name} must be a secret_ref")))?;
            out.query.push((name.clone(), text));
        }
    }
    Ok(out)
}

/// `min_interval_ms` with Zeus 08's bounds (200 ms to one hour), default `default_ms`.
///
/// # Errors
///
/// `op.config_invalid`.
pub fn min_interval(v: Option<&Value>, default_ms: u64) -> OpResult<Duration> {
    match v {
        None | Some(Value::Null) => Ok(Duration::from_millis(default_ms)),
        Some(v) => match v.as_u64() {
            Some(ms) if (200..=3_600_000).contains(&ms) => Ok(Duration::from_millis(ms)),
            _ => Err(config("min_interval_ms must be 200 to 3600000")),
        },
    }
}

/// Reads an optional list of allowed domains.
///
/// # Errors
///
/// `op.config_invalid`.
pub fn allowed_domains(v: Option<&Value>) -> OpResult<Option<domains::AllowedDomains>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let list: Vec<String> = items
                .iter()
                .map(|i| {
                    i.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| config("allowed_domains must be strings"))
                })
                .collect::<OpResult<_>>()?;
            domains::AllowedDomains::parse(&list).map(Some)
        }
        Some(_) => Err(config("allowed_domains must be a list of host names")),
    }
}
