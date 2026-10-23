//! The hub link: a self-hosted `ogun-server` over its REST routes, read-only, with an API token.
//!
//! This is how the iOS and Android companions (and a desktop pointed at a server) see a hub (O-12: Studio
//! "partial: companion" on mobile, "remote" for edges). API tokens always act as agents (ADR 0010), so this link
//! can never decide an approval; Studio also sends no mutating request over it. The token lives only in this
//! process (zeroized on drop) and in an owner-only file in the app's data directory; it is never returned to the
//! webview, logged or put in an error. TLS is rustls with ring: the Mozilla roots by default, or exactly the CA a
//! person pins for a hub with a private CA.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ogun_api::methods::{
    ApprovalRef, EventsSinceParams, GetArtifactParams, GetWorkflowParams, LineageParams,
    ListApprovalsParams, ListArtifactsParams, ListRunsParams, PartitionStatusParams,
    PlanBackfillParams, RunRef, StreamRef, WorkflowDocument, name,
};
use ogun_types::OgunError;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::error::{StudioError, StudioResult};

/// Largest JSON response read from the hub.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Largest artifact fetched inline for a preview.
pub const MAX_INLINE_BYTES: u64 = 1024 * 1024;

/// A connection to a hub.
pub struct HubLink {
    base: Url,
    token: Zeroizing<String>,
    http: reqwest::Client,
}

impl std::fmt::Debug for HubLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubLink")
            .field("base", &self.display_url())
            .finish_non_exhaustive()
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

/// Checks a hub URL: `https://host[:port][/prefix]`, or `http://` to a loopback host for development. No user
/// information, query or fragment.
///
/// # Errors
///
/// `api.invalid_params` naming the problem (never echoing the URL).
pub fn parse_hub_url(text: &str) -> StudioResult<Url> {
    let mut url =
        Url::parse(text.trim()).map_err(|_| StudioError::invalid("the hub URL does not parse"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(StudioError::invalid(
            "the hub URL must not contain credentials; use an API token",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(StudioError::invalid(
            "the hub URL must not have a query or fragment",
        ));
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&url) => {}
        _ => {
            return Err(StudioError::invalid(
                "the hub URL must be https (http only for a loopback address)",
            ));
        }
    }
    if url.host().is_none() {
        return Err(StudioError::invalid("the hub URL has no host"));
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn tls_config(ca_pem: Option<&str>) -> StudioResult<rustls::ClientConfig> {
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;

    let mut roots = rustls::RootCertStore::empty();
    match ca_pem.map(str::trim).filter(|p| !p.is_empty()) {
        Some(pem) => {
            let mut added = 0;
            for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
                let cert = cert.map_err(|_| StudioError::invalid("the pinned CA is not PEM"))?;
                roots.add(cert).map_err(|_| {
                    StudioError::invalid("the pinned CA is not a usable certificate")
                })?;
                added += 1;
            }
            if added == 0 {
                return Err(StudioError::invalid("the pinned CA has no certificate"));
            }
        }
        None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| StudioError::unavailable("no TLS protocol versions available"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(config)
}

fn token_ok(token: &str) -> bool {
    (16..=512).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

/// Maps a transport failure to an envelope without the URL (which reqwest errors otherwise carry).
fn transport(e: &reqwest::Error) -> StudioError {
    let what = if e.is_timeout() {
        "the hub did not answer in time"
    } else if e.is_connect() {
        "cannot connect to the hub"
    } else if e.is_decode() || e.is_body() {
        "the hub's answer could not be read"
    } else {
        "the request to the hub failed"
    };
    StudioError::unavailable(what)
}

fn status_error(status: StatusCode, body: &[u8]) -> StudioError {
    #[derive(serde::Deserialize)]
    struct Wrapped {
        error: OgunError,
    }
    if let Ok(w) = serde_json::from_slice::<Wrapped>(body) {
        return StudioError(w.error);
    }
    let code = match status.as_u16() {
        400 | 413 | 422 => "api.invalid_params",
        401 => "api.unauthenticated",
        403 => "api.permission_denied",
        404 => "api.method_not_found",
        409 => "api.idempotency_conflict",
        _ => "api.unavailable",
    };
    StudioError::new(code, format!("the hub answered HTTP {}", status.as_u16()))
}

fn typed<T: DeserializeOwned>(params: Value) -> StudioResult<T> {
    serde_json::from_value(params).map_err(|e| StudioError::invalid(format!("bad parameters: {e}")))
}

impl HubLink {
    /// Builds a link. Nothing is sent until the first call.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` for a bad URL, token or CA.
    pub fn new(url: &str, token: Zeroizing<String>, ca_pem: Option<&str>) -> StudioResult<Self> {
        let base = parse_hub_url(url)?;
        if !token_ok(&token) {
            return Err(StudioError::invalid(
                "the API token must be 16 to 512 printable ASCII characters",
            ));
        }
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls_config(ca_pem)?)
            .https_only(base.scheme() == "https")
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ogun-studio/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| transport(&e))?;
        Ok(Self { base, token, http })
    }

    /// The hub URL as shown to people: scheme, host, port and path only.
    #[must_use]
    pub fn display_url(&self) -> String {
        self.base.to_string()
    }

    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> StudioResult<Url> {
        let mut url = self.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| StudioError::invalid("the hub URL cannot take a path"))?;
            path.pop_if_empty().extend(["api", "v1"]).extend(segments);
        }
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        Ok(url)
    }

    fn auth(&self) -> StudioResult<HeaderValue> {
        let mut v = HeaderValue::from_str(&format!("Bearer {}", self.token.as_str()))
            .map_err(|_| StudioError::invalid("the API token is not a valid header value"))?;
        v.set_sensitive(true);
        Ok(v)
    }

    async fn read_json(&self, resp: reqwest::Response) -> StudioResult<Value> {
        let status = resp.status();
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return Err(StudioError::unavailable("the hub's answer is too large"));
        }
        let bytes = resp.bytes().await.map_err(|e| transport(&e))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(StudioError::unavailable("the hub's answer is too large"));
        }
        if !status.is_success() {
            return Err(status_error(status, &bytes));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| StudioError::unavailable("the hub's answer is not JSON"))
    }

    async fn get(&self, segments: &[&str], query: &[(&str, String)]) -> StudioResult<Value> {
        let resp = self
            .http
            .get(self.url(segments, query)?)
            .header(AUTHORIZATION, self.auth()?)
            .header(ACCEPT, "application/json")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| transport(&e))?;
        self.read_json(resp).await
    }

    async fn post(&self, segments: &[&str], body: &Value) -> StudioResult<Value> {
        let resp = self
            .http
            .post(self.url(segments, &[])?)
            .header(AUTHORIZATION, self.auth()?)
            .header(ACCEPT, "application/json")
            .json(body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| transport(&e))?;
        self.read_json(resp).await
    }

    /// Calls a read method through its REST route (the table in the Studio-web REST contract).
    ///
    /// # Errors
    ///
    /// `api.permission_denied` for anything that is not a read; the hub's envelope otherwise.
    pub async fn call(&self, method: &str, params: Value) -> StudioResult<Value> {
        match method {
            name::DAEMON_STATUS => self.get(&["status"], &[]).await,
            name::WORKFLOWS_LIST => self.get(&["workflows"], &[]).await,
            name::WORKFLOWS_GET => {
                let p: GetWorkflowParams = typed(params)?;
                let q: Vec<(&str, String)> = p
                    .version
                    .map(|v| ("version", v.to_string()))
                    .into_iter()
                    .collect();
                self.get(&["workflows", &p.name], &q).await
            }
            name::WORKFLOWS_VALIDATE => {
                let p: WorkflowDocument = typed(params)?;
                self.post(
                    &["workflows", "validate"],
                    &json!({ "workflow": p.workflow }),
                )
                .await
            }
            name::RUNS_LIST => {
                let p: ListRunsParams = typed(params)?;
                let mut q = Vec::new();
                if let Some(w) = p.workflow {
                    q.push(("workflow", w));
                }
                if let Some(b) = p.before {
                    q.push(("before", b.to_string()));
                }
                if let Some(l) = p.limit {
                    q.push(("limit", l.to_string()));
                }
                self.get(&["runs"], &q).await
            }
            name::RUNS_GET => {
                let p: RunRef = typed(params)?;
                self.get(&["runs", &p.run_id.to_string()], &[]).await
            }
            name::PARTITIONS_STATUS => {
                let p: PartitionStatusParams = typed(params)?;
                let mut q = Vec::new();
                if let Some(v) = p.version {
                    q.push(("version", v.to_string()));
                }
                if let Some(f) = p.from {
                    q.push(("from", f));
                }
                if let Some(t) = p.to {
                    q.push(("to", t));
                }
                self.get(&["workflows", &p.workflow, "partitions"], &q)
                    .await
            }
            name::BACKFILLS_PLAN => {
                let p: PlanBackfillParams = typed(params)?;
                self.post(
                    &["workflows", &p.workflow, "backfill-plan"],
                    &json!({ "from": p.from, "to": p.to }),
                )
                .await
            }
            name::EVENTS_SINCE => self.events(typed(params)?).await,
            name::APPROVALS_LIST => {
                let p: ListApprovalsParams = typed(params)?;
                let q: Vec<(&str, String)> = p
                    .state
                    .and_then(|s| serde_json::to_value(s).ok())
                    .and_then(|v| v.as_str().map(|s| ("state", s.to_owned())))
                    .into_iter()
                    .collect();
                self.get(&["approvals"], &q).await
            }
            name::APPROVALS_GET => {
                let p: ApprovalRef = typed(params)?;
                self.get(&["approvals", &p.approval_id.to_string()], &[])
                    .await
            }
            name::ARTIFACTS_LIST => {
                let p: ListArtifactsParams = typed(params)?;
                self.get(&["artifacts"], &[("run_id", p.run_id.to_string())])
                    .await
            }
            name::ARTIFACTS_GET => self.artifact(typed(params)?).await,
            name::LINEAGE_GET => {
                let p: LineageParams = typed(params)?;
                let mut q = Vec::new();
                if let Some(r) = p.run_id {
                    q.push(("run_id", r.to_string()));
                }
                if let Some(b) = p.blake3 {
                    q.push(("blake3", b));
                }
                self.get(&["lineage"], &q).await
            }
            name::STREAMS_LIST => self.get(&["streams"], &[]).await,
            name::STREAMS_STATUS => {
                let p: StreamRef = typed(params)?;
                self.get(&["streams", &p.workflow, &p.node], &[]).await
            }
            name::EDGES_LIST => self.get(&["edges"], &[]).await,
            _ => Err(StudioError::denied(
                "a hub link is read-only in Studio; use the desktop or Studio-web for this",
            )),
        }
    }

    /// `artifacts.get`: the metadata route, plus the content route when a small inline preview is asked for.
    async fn artifact(&self, p: GetArtifactParams) -> StudioResult<Value> {
        let meta = self.get(&["artifacts", &p.blake3], &[]).await?;
        let mut result = if meta.get("artifact").is_some() {
            meta
        } else {
            json!({ "artifact": meta, "path": "" })
        };
        let size = result["artifact"]["size_bytes"]
            .as_u64()
            .unwrap_or(u64::MAX);
        let max = p.inline_max_bytes.min(MAX_INLINE_BYTES);
        if max > 0 && size <= max && result.get("content_base64").is_none() {
            let resp = self
                .http
                .get(self.url(&["artifacts", &p.blake3, "content"], &[])?)
                .header(AUTHORIZATION, self.auth()?)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| transport(&e))?;
            let status = resp.status();
            let bytes = resp.bytes().await.map_err(|e| transport(&e))?;
            if !status.is_success() {
                return Err(status_error(status, &bytes));
            }
            if bytes.len() as u64 <= max {
                result["content_base64"] = Value::String(STANDARD.encode(&bytes));
            }
        }
        Ok(result)
    }

    /// `events.since` over the SSE route: reads `event: ogun` frames until the first batch has arrived and the
    /// stream is quiet, or the wait elapses, then closes the stream.
    async fn events(&self, p: EventsSinceParams) -> StudioResult<Value> {
        let mut q = vec![("after", p.after.to_string())];
        if let Some(r) = p.run_id {
            q.push(("run_id", r.to_string()));
        }
        let limit = usize::try_from(p.limit.unwrap_or(100).clamp(1, 1000)).unwrap_or(100);
        let wait = Duration::from_millis(p.wait_ms.clamp(250, 30_000));
        let mut resp = self
            .http
            .get(self.url(&["events"], &q)?)
            .header(AUTHORIZATION, self.auth()?)
            .header(ACCEPT, "text/event-stream")
            .timeout(wait + Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| transport(&e))?;
        let status = resp.status();
        if !status.is_success() {
            let bytes = resp.bytes().await.unwrap_or_default();
            return Err(status_error(status, &bytes));
        }
        let deadline = Instant::now() + wait;
        let mut pending = String::new();
        let (mut event, mut data) = (String::new(), String::new());
        let mut events: Vec<Value> = Vec::new();
        'read: while events.len() < limit {
            let budget = if events.is_empty() {
                deadline.saturating_duration_since(Instant::now())
            } else {
                Duration::from_millis(200)
            };
            if budget.is_zero() {
                break;
            }
            let chunk = match tokio::time::timeout(budget, resp.chunk()).await {
                Err(_) | Ok(Ok(None)) => break,
                Ok(Err(e)) if events.is_empty() => return Err(transport(&e)),
                Ok(Err(_)) => break,
                Ok(Ok(Some(c))) => c,
            };
            pending.push_str(&String::from_utf8_lossy(&chunk));
            if pending.len() > MAX_RESPONSE_BYTES {
                return Err(StudioError::unavailable(
                    "the hub's event frame is too large",
                ));
            }
            while let Some(nl) = pending.find('\n') {
                let line: String = pending.drain(..=nl).collect();
                let line = line.trim_end_matches(['\n', '\r']);
                if line.is_empty() {
                    if (event.is_empty() || event == "ogun")
                        && !data.is_empty()
                        && let Ok(v) = serde_json::from_str::<Value>(&data)
                        && v["seq"].as_u64().is_some_and(|s| s > p.after)
                    {
                        events.push(v);
                    }
                    event.clear();
                    data.clear();
                    if events.len() >= limit {
                        break 'read;
                    }
                } else if let Some(rest) = line.strip_prefix("event:") {
                    event = rest.trim().to_owned();
                } else if let Some(rest) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                }
            }
        }
        let last_seq = events
            .iter()
            .filter_map(|e| e["seq"].as_u64())
            .max()
            .unwrap_or(p.after);
        Ok(json!({ "events": events, "last_seq": last_seq }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_urls_are_https_without_credentials() {
        assert!(parse_hub_url("https://hub.example.org").is_ok());
        assert_eq!(
            parse_hub_url("https://hub.example.org/ogun")
                .unwrap()
                .as_str(),
            "https://hub.example.org/ogun/"
        );
        assert!(parse_hub_url("http://127.0.0.1:8443").is_ok());
        assert!(parse_hub_url("http://hub.example.org").is_err());
        assert!(parse_hub_url("https://user:pw@hub.example.org").is_err());
        assert!(parse_hub_url("https://hub.example.org/?token=x").is_err());
        assert!(parse_hub_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn routes_are_built_under_api_v1_and_tokens_stay_out_of_debug() {
        let link = HubLink::new(
            "https://hub.example.org/ogun",
            Zeroizing::new("tok_ZEUS-CANARY-0123456789".into()),
            None,
        )
        .unwrap();
        let url = link
            .url(
                &["workflows", "a b", "partitions"],
                &[("from", "w1".into())],
            )
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://hub.example.org/ogun/api/v1/workflows/a%20b/partitions?from=w1"
        );
        assert!(!format!("{link:?}").contains("ZEUS-CANARY-"));
    }
}
