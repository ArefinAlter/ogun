//! Fetching one document (a page, an API answer, an S3 listing) through a [`ScopedTransport`],
//! following redirects itself so every hop passes the scope's checks.

use url::Url;
use zeus_http::{HttpTransport, TransportError, TransportFailure};

use super::transport::{HOST_NOT_ALLOWED, NETWORK_OFF, ROBOTS_DISALLOWED, read_body, request};
use crate::error::OpError;
use crate::secrets::redact;

/// Redirects followed per document.
const MAX_REDIRECTS: usize = 10;

/// A fetched document.
#[derive(Debug, Clone)]
pub struct Document {
    /// The URL after redirects (without injected query secrets).
    pub url: Url,
    /// HTTP status.
    pub status: u16,
    /// `Content-Type`, lowercased, without parameters.
    pub content_type: String,
    /// The `Link` header, if any.
    pub link: Option<String>,
    /// The body.
    pub body: Vec<u8>,
}

/// Maps a transport failure to an operator error. Messages carry the redacted URL only.
#[must_use]
pub fn transport_error(e: &TransportError, url: &Url) -> OpError {
    let at = redact(url.as_str());
    match (e.kind, e.detail.as_str()) {
        (TransportFailure::PolicyDenied, NETWORK_OFF) => OpError::new(
            "op.network_denied",
            "network access is off in this environment",
        ),
        (TransportFailure::PolicyDenied, HOST_NOT_ALLOWED) => OpError::new(
            "op.network_denied",
            format!("{at} is outside allowed_domains"),
        ),
        (TransportFailure::PolicyDenied, ROBOTS_DISALLOWED) => {
            OpError::new("op.fetch_refused", format!("robots.txt disallows {at}"))
        }
        (TransportFailure::TlsCertificate, _) => OpError::new(
            "op.fetch_refused",
            format!("the TLS certificate of {at} does not verify"),
        ),
        (TransportFailure::Other, "too_large") => {
            crate::error::limit(format!("the document at {at} is larger than the limit"))
        }
        (kind, _) => OpError::new(
            "op.fetch_failed",
            format!("fetching {at} failed ({kind:?})"),
        ),
    }
}

/// GETs `url`, following up to ten redirects, and reads at most `max_bytes` of the body.
///
/// # Errors
///
/// `op.network_denied`, `op.fetch_refused`, `op.fetch_failed`, `op.limit_exceeded`.
pub async fn get(
    transport: &dyn HttpTransport,
    url: &Url,
    accept: &str,
    max_bytes: usize,
) -> Result<Document, OpError> {
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let response = transport
            .send(request(current.clone(), accept))
            .await
            .map_err(|e| transport_error(&e, &current))?;
        let status = response.status.as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let next = response
                .headers
                .get(http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| response.url.join(l).ok())
                .ok_or_else(|| {
                    OpError::new(
                        "op.fetch_refused",
                        format!(
                            "{} redirected without a usable Location",
                            redact(current.as_str())
                        ),
                    )
                })?;
            current = next;
            continue;
        }
        let content_type = response
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        let link = response
            .headers
            .get(http::header::LINK)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let final_url = response.url.clone();
        let body = read_body(response, max_bytes)
            .await
            .map_err(|e| transport_error(&e, &final_url))?;
        return Ok(Document {
            url: final_url,
            status,
            content_type,
            link,
            body,
        });
    }
    Err(OpError::new(
        "op.fetch_refused",
        format!("{} redirected more than ten times", redact(url.as_str())),
    ))
}

/// Builds the production transport of zeus-core with Ogun's honest user agent and no proxy surprises.
///
/// # Errors
///
/// `internal.bug` when the TLS client cannot be built.
pub fn production_transport() -> Result<std::sync::Arc<dyn HttpTransport>, OpError> {
    let config = zeus_http::ClientConfig {
        user_agent: zeus_http::UserAgentPolicy::Custom(user_agent()),
        ..Default::default()
    };
    zeus_http::ReqwestTransport::new(&config)
        .map(|t| std::sync::Arc::new(t) as std::sync::Arc<dyn HttpTransport>)
        .map_err(|_| crate::error::internal("the HTTP client could not be built"))
}

/// `Ogun/<version> (+https://github.com/ArefinAlter/ogun)`: an honest automated client.
#[must_use]
pub fn user_agent() -> String {
    format!(
        "Ogun/{} (+https://github.com/ArefinAlter/ogun)",
        env!("CARGO_PKG_VERSION")
    )
}
