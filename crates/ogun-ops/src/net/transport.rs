//! The scoped HTTP transport every Ogun network operator uses.
//!
//! It wraps the zeus-core production transport (reqwest and rustls; no redirects, no decompression, no
//! cookie store) and, on **every** request (so on every redirect hop and range retry the engine makes):
//!
//! - refuses everything when the environment has no network access (benchmark profiles, ADR 0005);
//! - refuses schemes other than http(s) and hosts outside the allowed domains;
//! - honours robots.txt for the product token `ogun`, fetched once per origin;
//! - spaces requests to one host by `min_interval_ms`;
//! - adds the operator's configured headers and query parameters (resolved secrets) only to allowed
//!   hosts, marks secret headers sensitive, and gives the response back with the request URL as it was
//!   before the query secrets were added, so no URL the engine records or logs ever carries them; the
//!   same parameters are removed from any `Location` header.
//!
//! It never adds cookies or any session material (constraint 2): session-bound sources are
//! `zeus.session_acquire` and go through `zeusd`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use http::header::{HeaderName, HeaderValue};
use tokio::time::Instant;
use url::Url;
use zeroize::Zeroizing;
use zeus_http::{HttpRequest, HttpResponse, HttpTransport, TransportError, TransportFailure};

use super::domains::AllowedDomains;
use super::robots::Robots;

/// `TransportError::detail` for a host outside the allowed domains.
pub const HOST_NOT_ALLOWED: &str = "host_not_allowed";
/// `TransportError::detail` for a path robots.txt disallows.
pub const ROBOTS_DISALLOWED: &str = "robots_disallowed";
/// `TransportError::detail` when the environment has no network access.
pub const NETWORK_OFF: &str = "network_off";

/// Headers and query parameters added to requests to allowed hosts.
#[derive(Default)]
pub struct Injection {
    /// Header names and values.
    pub headers: Vec<(HeaderName, Zeroizing<String>)>,
    /// Query parameter names and values.
    pub query: Vec<(String, Zeroizing<String>)>,
}

impl std::fmt::Debug for Injection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Injection")
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>(),
            )
            .field(
                "query",
                &self
                    .query
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// The policy of one operator attempt.
#[derive(Debug)]
pub struct Scope {
    /// Hosts that may be contacted.
    pub allowed: AllowedDomains,
    /// Added to every request to an allowed host.
    pub inject: Injection,
    /// Minimum spacing between request starts to one host.
    pub min_interval: Duration,
    /// Whether the environment has network access at all.
    pub network: bool,
    /// Byte budget for committed download bytes (`acquire`).
    pub max_bytes: Option<u64>,
    /// Bytes committed so far.
    pub committed: AtomicU64,
    robots: tokio::sync::Mutex<HashMap<String, Arc<Robots>>>,
    last: Mutex<HashMap<String, Instant>>,
}

impl Scope {
    /// A scope.
    #[must_use]
    pub fn new(
        allowed: AllowedDomains,
        inject: Injection,
        min_interval: Duration,
        network: bool,
    ) -> Self {
        Self {
            allowed,
            inject,
            min_interval,
            network,
            max_bytes: None,
            committed: AtomicU64::new(0),
            robots: tokio::sync::Mutex::new(HashMap::new()),
            last: Mutex::new(HashMap::new()),
        }
    }

    async fn robots_for(&self, inner: &dyn HttpTransport, url: &Url) -> Arc<Robots> {
        let origin = url.origin().ascii_serialization();
        let mut cache = self.robots.lock().await;
        if let Some(r) = cache.get(&origin) {
            return r.clone();
        }
        let r = Arc::new(fetch_robots(inner, &origin, self).await);
        cache.insert(origin, r.clone());
        r
    }

    async fn pace(&self, host: &str) {
        if self.min_interval.is_zero() {
            return;
        }
        let wait = {
            let mut last = self
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let slot = match last.get(host) {
                Some(prev) if *prev + self.min_interval > now => *prev + self.min_interval,
                _ => now,
            };
            last.insert(host.to_owned(), slot);
            slot.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

fn denied(detail: &'static str) -> TransportError {
    TransportError::new(TransportFailure::PolicyDenied, detail)
}

async fn fetch_robots(inner: &dyn HttpTransport, origin: &str, scope: &Scope) -> Robots {
    let Ok(mut url) = Url::parse(&format!("{origin}/robots.txt")) else {
        return Robots::disallow_all();
    };
    for _ in 0..5 {
        scope.pace(url.host_str().unwrap_or("")).await;
        let response = inner.send(request(url.clone(), "text/plain")).await;
        let Ok(response) = response else {
            return Robots::disallow_all();
        };
        let status = response.status.as_u16();
        if (300..400).contains(&status) {
            let next = response
                .headers
                .get(http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| url.join(l).ok());
            match next {
                Some(n) if scope.allowed.allows_url(&n) => {
                    url = n;
                    continue;
                }
                // A redirect off the allowed hosts: treat as unavailable (allow, RFC 9309 2.3.1.2 says
                // follow; Ogun may not, so it falls back to the "no robots.txt" rule).
                _ => return Robots::allow_all(),
            }
        }
        return match status {
            200..=299 => match read_body(response, 500 * 1024).await {
                Ok(body) => Robots::parse(&String::from_utf8_lossy(&body)),
                Err(_) => Robots::disallow_all(),
            },
            429 => Robots::disallow_all(),
            400..=499 => Robots::allow_all(),
            _ => Robots::disallow_all(),
        };
    }
    Robots::allow_all()
}

/// A GET request with the usual timeouts.
#[must_use]
pub fn request(url: Url, accept: &str) -> HttpRequest {
    let mut headers = http::HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(accept) {
        headers.insert(http::header::ACCEPT, v);
    }
    headers.insert(
        http::header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    HttpRequest {
        method: http::Method::GET,
        url,
        headers,
        head_timeout: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(30),
    }
}

/// Reads a body up to `max` bytes.
///
/// # Errors
///
/// A transport error, or `Other` with detail `too_large` beyond `max`.
pub async fn read_body(mut response: HttpResponse, max: usize) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::new();
    while let Some(chunk) = response.body.next().await {
        let chunk = chunk?;
        if out.len() + chunk.len() > max {
            return Err(TransportError::new(TransportFailure::Other, "too_large"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// The scoped transport.
pub struct ScopedTransport {
    inner: Arc<dyn HttpTransport>,
    scope: Arc<Scope>,
}

impl ScopedTransport {
    /// Wraps `inner` with `scope`.
    #[must_use]
    pub fn new(inner: Arc<dyn HttpTransport>, scope: Arc<Scope>) -> Self {
        Self { inner, scope }
    }

    /// The scope.
    #[must_use]
    pub fn scope(&self) -> &Arc<Scope> {
        &self.scope
    }
}

impl std::fmt::Debug for ScopedTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedTransport")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

fn strip_params(url: &mut Url, names: &[&str]) {
    if url.query().is_none() || names.is_empty() {
        return;
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !names.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
}

#[async_trait]
impl HttpTransport for ScopedTransport {
    async fn send(&self, mut request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let scope = &self.scope;
        if !scope.network {
            return Err(denied(NETWORK_OFF));
        }
        if !scope.allowed.allows_url(&request.url) {
            return Err(denied(HOST_NOT_ALLOWED));
        }
        let robots = scope.robots_for(self.inner.as_ref(), &request.url).await;
        let path = match request.url.query() {
            Some(q) => format!("{}?{q}", request.url.path()),
            None => request.url.path().to_owned(),
        };
        if !robots.allows(&path) {
            return Err(denied(ROBOTS_DISALLOWED));
        }
        scope.pace(request.url.host_str().unwrap_or("")).await;
        let original = request.url.clone();
        for (name, value) in &scope.inject.headers {
            if let Ok(mut v) = HeaderValue::from_str(value) {
                v.set_sensitive(true);
                request.headers.insert(name.clone(), v);
            }
        }
        if !scope.inject.query.is_empty() {
            let names: Vec<&str> = scope.inject.query.iter().map(|(n, _)| n.as_str()).collect();
            strip_params(&mut request.url, &names);
            let mut pairs = request.url.query_pairs_mut();
            for (name, value) in &scope.inject.query {
                pairs.append_pair(name, value);
            }
        }
        let mut response = self.inner.send(request).await?;
        response.url = original;
        if !scope.inject.query.is_empty()
            && let Some(location) = response
                .headers
                .get(http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            && let Ok(mut target) = response.url.join(location)
        {
            let names: Vec<&str> = scope.inject.query.iter().map(|(n, _)| n.as_str()).collect();
            strip_params(&mut target, &names);
            if let Ok(v) = HeaderValue::from_str(target.as_str()) {
                response.headers.insert(http::header::LOCATION, v);
            }
        }
        Ok(response)
    }
}

/// Counts committed bytes against the scope's budget; `false` once it is exceeded.
#[must_use]
pub fn commit_bytes(scope: &Scope, n: u64) -> bool {
    let total = scope
        .committed
        .fetch_add(n, Ordering::AcqRel)
        .saturating_add(n);
    scope.max_bytes.is_none_or(|max| total <= max)
}
