//! `acquire`: plain HTTP(S) downloads through the zeus-core engine (ADR 0002; O-02 section 6.2 rule 4).
//!
//! The registry owns one [`zeus_engine::Engine`], built on first use with:
//!
//! - its journal and state under `<state_dir>/ops/acquire/` and its only write root
//!   `<state_dir>/ops/acquire/downloads/` (one directory per node instance);
//! - a session provider and proxy-fetch port that always answer "no session" ([`NoSession`]):
//!   constraint 2, Ogun never holds browser session material; a source behind a login is
//!   `zeus.session_acquire`, which runs through `zeusd`;
//! - no approvals port, so anything that would need a human decision fails closed;
//! - a per-job transport factory that scopes every request of a node instance's jobs to its allowed
//!   domains, robots.txt, pacing and configured header and query secrets ([`crate::net::transport`]), and
//!   counts committed bytes against `budgets.max_bytes`.
//!
//! Jobs are keyed by node instance and URL, so a later attempt of the same node instance finds them
//! again: a failed job is retried and a paused one resumed from its committed bytes (the engine's
//! resumability); an attempt that times out pauses its jobs instead of discarding them. Files move from
//! the engine's download directory into `scratch/artifacts/` only when every job of the attempt has
//! completed (or `allow_partial` is set), so a failed attempt keeps what it already has.
//!
//! Configuration (the portable acquisition vocabulary of Zeus 08 that applies to named files):
//!
//! | Member | Meaning |
//! |---|---|
//! | `allowed_domains` | Hosts that may be contacted; default: the exact hosts of the URLs. |
//! | `sources` | `[{"url", "filename", "sha256", "size_bytes"}]` or Zeus 08 `{"type": "start_urls", "urls"}`. Inputs (JSON Lines rows with `url`, or text with one URL per line, from `crawl.urls`) add more. |
//! | `listing` | `"s3_prefix"`: each URL is an S3 prefix; its objects are listed (ListObjectsV2) and downloaded. |
//! | `headers`, `query_secrets` | Added by the scoped transport to requests to allowed hosts; values may be `secret_ref`s and never enter the engine's journal. |
//! | `fetch` | `min_interval_ms` (default 0), `max_connections_per_file` (1 to 8, default 4), `download_accept`. |
//! | `budgets` | `max_files` (default 10 000), `max_bytes`. |
//! | `attribution` | Text recorded in every artifact's metadata. |
//! | `allow_partial` | Succeed with the files that completed. Default `false`. |
//!
//! `pagination`, `extract`, `filters` and `stop` (page discovery) are `op.unsupported` here: discovery is
//! `crawl@1`. `session` and `infinite_scroll` are refused (they need a browser).

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tokio::sync::OnceCell;
use url::Url;
use zeus_engine::{Caller, ControlAction, Engine, EngineBuilder, EngineConfig, EnqueueRequest};
use zeus_session::{
    LeaseRequest, ProxyFetch, ProxyFetchRequest, ProxyResponse, RefreshedUrl, SessionError,
    SessionLease, SessionProvider, SessionResult, UrlRefreshRequest,
};
use zeus_types::{
    ActorKind, ErrorEnvelope, HttpUrl, JobId, JobSpec, JobState, Source, SourceKind, ToEnvelope,
};

use crate::error::{OpError, OpResult, config, internal, unsupported};
use crate::net::domains::AllowedDomains;
use crate::net::fetch::{get, production_transport, user_agent};
use crate::net::transport::{Scope, ScopedTransport, commit_bytes};
use crate::net::{allowed_domains, injection, min_interval};
use crate::ops::{CoreFail, CoreResult, Job};
use crate::secrets::{ResolvedSecrets, redact, redact_record};
use crate::{Environment, OutputFile, files};

const SCOPE_LABEL: &str = "ogun_scope";
const CLIENT_ID: &str = "ogun-ops:acquire";

/// A session provider and proxy-fetch port that never have a session (constraint 2).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSession;

#[async_trait]
impl SessionProvider for NoSession {
    async fn lease(&self, _request: LeaseRequest) -> SessionResult<SessionLease> {
        Err(SessionError::ExtensionUnavailable)
    }
    async fn refresh_url(&self, _request: UrlRefreshRequest) -> SessionResult<RefreshedUrl> {
        Err(SessionError::ExtensionUnavailable)
    }
    fn extension_connected(&self, _profile_id: Option<&str>) -> bool {
        false
    }
}

#[async_trait]
impl ProxyFetch for NoSession {
    async fn fetch_range(&self, _request: ProxyFetchRequest) -> SessionResult<ProxyResponse> {
        Err(SessionError::ExtensionUnavailable)
    }
    fn available(&self, _profile_id: Option<&str>) -> bool {
        false
    }
}

/// Scopes per node instance, handed to the engine as its job transport factory.
struct Scopes {
    inner: Arc<dyn zeus_http::HttpTransport>,
    map: Mutex<HashMap<String, Arc<Scope>>>,
}

impl Scopes {
    fn get(&self, spec: &JobSpec) -> Option<Arc<Scope>> {
        let id = spec.labels.get(SCOPE_LABEL)?;
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }
}

impl zeus_engine::JobTransportFactory for Scopes {
    fn for_job(
        &self,
        spec: &JobSpec,
    ) -> Result<Option<Arc<dyn zeus_http::HttpTransport>>, ErrorEnvelope> {
        match self.get(spec) {
            Some(scope) => Ok(Some(Arc::new(ScopedTransport::new(
                self.inner.clone(),
                scope,
            )))),
            None => Err(ErrorEnvelope::from_code(
                zeus_types::codes::POLICY_APPROVAL_DENIED,
                "no active Ogun scope for this job; it resumes when its node instance runs again",
            )),
        }
    }

    fn validate_commit(
        &self,
        spec: &JobSpec,
        _txn: &dyn zeus_journal::ReadTxn,
        range: zeus_types::ByteRange,
    ) -> Result<(), ErrorEnvelope> {
        let Some(scope) = self.get(spec) else {
            return Err(ErrorEnvelope::from_code(
                zeus_types::codes::POLICY_APPROVAL_DENIED,
                "no active Ogun scope for this job",
            ));
        };
        if commit_bytes(&scope, range.len()) {
            Ok(())
        } else {
            Err(ErrorEnvelope::from_code(
                zeus_types::codes::WORKFLOW_BUDGET_EXHAUSTED,
                "the node's max_bytes budget is exhausted",
            ))
        }
    }
}

/// The engine `acquire` runs on, built on first use.
#[derive(Default)]
pub struct AcquireEngine {
    cell: OnceCell<Arc<Built>>,
}

impl std::fmt::Debug for AcquireEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcquireEngine")
            .field("started", &self.cell.initialized())
            .finish()
    }
}

struct Built {
    engine: Engine,
    downloads: PathBuf,
    scopes: Arc<Scopes>,
}

impl AcquireEngine {
    async fn get(&self, env: &Environment) -> OpResult<Arc<Built>> {
        self.cell
            .get_or_try_init(|| async {
                let base = env.state_dir.join("ops").join("acquire");
                let downloads = base.join("downloads");
                std::fs::create_dir_all(&downloads)
                    .map_err(|e| crate::error::io("creating the acquire state", &e))?;
                let clock: zeus_control::SharedClock = Arc::new(zeus_control::SystemClock::new());
                let journal = zeus_journal::Journal::open(
                    &base.join("journal.redb"),
                    &zeus_journal::JournalOptions::default(),
                    clock.clone(),
                )
                .map_err(|e| engine_error(&e.to_envelope(), "opening the acquire journal"))?;
                let mut config =
                    EngineConfig::new(vec![downloads.clone()], downloads.clone(), base.clone());
                config.http.user_agent = zeus_http::UserAgentPolicy::Custom(user_agent());
                config.zeus_version = format!("ogun-ops/{}", env!("CARGO_PKG_VERSION"));
                let scopes = Arc::new(Scopes {
                    inner: production_transport()?,
                    map: Mutex::new(HashMap::new()),
                });
                let no = Arc::new(NoSession);
                let engine = EngineBuilder::new(config)
                    .clock(clock)
                    .journal(journal)
                    .session_provider(no.clone())
                    .proxy_fetch(no)
                    .job_transports(scopes.clone())
                    .build()
                    .await
                    .map_err(|e| engine_error(&e.to_envelope(), "starting the acquire engine"))?;
                Ok(Arc::new(Built {
                    engine,
                    downloads,
                    scopes,
                }))
            })
            .await
            .cloned()
    }
}

fn engine_error(env: &ErrorEnvelope, what: &str) -> OpError {
    use zeus_types::ErrorClass as C;
    let code = match env.class {
        C::TransientNetwork | C::Throttled | C::Timeout | C::Unavailable | C::UrlExpired => {
            "op.fetch_failed"
        }
        C::Integrity => "op.integrity",
        C::Disk => "op.io",
        C::PolicyDenied if env.code.as_str() == zeus_types::codes::WORKFLOW_BUDGET_EXHAUSTED => {
            "op.limit_exceeded"
        }
        C::PolicyDenied => "op.fetch_refused",
        C::InvalidRequest => "op.config_invalid",
        C::Internal => "internal.bug",
        _ => "op.fetch_refused",
    };
    let mut details = Map::new();
    details.insert("zeus_code".into(), json!(env.code.as_str()));
    details.insert("zeus_class".into(), json!(env.class.as_str()));
    OpError::new(
        code,
        format!("{what}: {} ({})", env.code.as_str(), env.class.as_str()),
    )
    .with_details(details)
}

#[derive(Debug, Clone)]
struct Wanted {
    url: Url,
    filename: Option<String>,
    sha256: Option<String>,
}

fn wanted_from_sources(v: Option<&Value>) -> OpResult<Vec<Wanted>> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let items = v
        .as_array()
        .ok_or_else(|| config("acquire: sources must be a list"))?;
    if items.len() > 16 && items.iter().any(|i| i.get("type").is_some()) {
        return Err(config("acquire: at most 16 Zeus sources"));
    }
    let parse = |s: &str| {
        Url::parse(s)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .ok_or_else(|| config("acquire: a source URL is not an absolute http(s) URL"))
    };
    let mut out = Vec::new();
    for item in items {
        let o = item
            .as_object()
            .ok_or_else(|| config("acquire: a source must be an object"))?;
        match o.get("type").and_then(Value::as_str) {
            None => {
                let url = o
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| config("acquire: a source needs url"))?;
                if let Some(k) = o
                    .keys()
                    .find(|k| !["url", "filename", "sha256", "size_bytes"].contains(&k.as_str()))
                {
                    return Err(config(format!(
                        "acquire: a source does not know the member {k}"
                    )));
                }
                out.push(Wanted {
                    url: parse(url)?,
                    filename: o.get("filename").and_then(Value::as_str).map(str::to_owned),
                    sha256: o
                        .get("sha256")
                        .and_then(Value::as_str)
                        .map(|s| s.trim_start_matches("sha256:").to_ascii_lowercase()),
                });
            }
            Some("start_urls") => {
                for u in o
                    .get("urls")
                    .and_then(Value::as_array)
                    .ok_or_else(|| config("acquire: start_urls needs urls"))?
                {
                    let s = u
                        .as_str()
                        .ok_or_else(|| config("acquire: urls must be strings"))?;
                    out.push(Wanted {
                        url: parse(s)?,
                        filename: None,
                        sha256: None,
                    });
                }
            }
            Some(_) => {
                return Err(unsupported(
                    "acquire downloads named files; api_endpoint and sitemap sources are discovery, use crawl@1",
                ));
            }
        }
    }
    Ok(out)
}

fn wanted_from_inputs(job: &Job) -> OpResult<Vec<Wanted>> {
    let mut out = Vec::new();
    for (i, input) in job.inputs.iter().enumerate() {
        let f = std::fs::File::open(&input.path)
            .map_err(|e| crate::error::io("reading an input", &e))?;
        for line in std::io::BufReader::new(f).lines() {
            let line = line.map_err(|e| crate::error::io("reading an input", &e))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let text = if line.starts_with('{') {
                let v: Value = serde_json::from_str(line).map_err(|_| {
                    crate::error::input(format!("acquire: a row of input {} is not JSON", i + 1))
                })?;
                match v.get("url").and_then(Value::as_str) {
                    Some(u) => u.to_owned(),
                    None => continue,
                }
            } else {
                line.to_owned()
            };
            let url = Url::parse(&text)
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https"))
                .ok_or_else(|| {
                    crate::error::input(format!(
                        "acquire: input {} holds a value that is not an http(s) URL",
                        i + 1
                    ))
                })?;
            out.push(Wanted {
                url,
                filename: None,
                sha256: None,
            });
            if out.len() > 1_000_000 {
                return Err(crate::error::limit("acquire: more than 1000000 input URLs"));
            }
        }
    }
    Ok(out)
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn xml_values(body: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(xml_unescape(&after[..j]));
        rest = &after[j + close.len()..];
    }
    out
}

/// Lists an S3 prefix with ListObjectsV2 (unauthenticated, or with the configured headers).
async fn list_s3(
    transport: &ScopedTransport,
    prefix_url: &Url,
    max: usize,
) -> OpResult<Vec<Wanted>> {
    let host = prefix_url.host_str().unwrap_or_default().to_owned();
    let path_style = host.starts_with("s3.") || host.starts_with("s3-");
    let raw_path = prefix_url.path().trim_start_matches('/');
    let (bucket_path, prefix) = if path_style {
        let (bucket, rest) = raw_path.split_once('/').unwrap_or((raw_path, ""));
        (format!("/{bucket}"), rest.to_owned())
    } else {
        ("/".to_owned(), raw_path.to_owned())
    };
    let prefix = percent_decode(&prefix);
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..10_000 {
        let mut list = prefix_url.clone();
        list.set_path(&bucket_path);
        list.set_query(None);
        {
            let mut q = list.query_pairs_mut();
            q.append_pair("list-type", "2")
                .append_pair("prefix", &prefix);
            if let Some(t) = &token {
                q.append_pair("continuation-token", t);
            }
        }
        let doc = get(transport, &list, "application/xml", 16 << 20).await?;
        if !(200..300).contains(&doc.status) {
            return Err(OpError::new(
                if doc.status >= 500 || doc.status == 429 {
                    "op.fetch_failed"
                } else {
                    "op.fetch_refused"
                },
                format!(
                    "listing {} answered {}",
                    redact(prefix_url.as_str()),
                    doc.status
                ),
            ));
        }
        let body = String::from_utf8_lossy(&doc.body);
        for key in xml_values(&body, "Key") {
            if key.ends_with('/') {
                continue;
            }
            let mut object = prefix_url.clone();
            object.set_query(None);
            let encoded: Vec<String> = key
                .split('/')
                .map(|seg| {
                    seg.bytes()
                        .map(|b| {
                            if b.is_ascii_alphanumeric()
                                || matches!(b, b'-' | b'.' | b'_' | b'~' | b'=')
                            {
                                char::from(b).to_string()
                            } else {
                                format!("%{b:02X}")
                            }
                        })
                        .collect()
                })
                .collect();
            let path = if path_style {
                format!("{bucket_path}/{}", encoded.join("/"))
            } else {
                format!("/{}", encoded.join("/"))
            };
            object.set_path(&path);
            out.push(Wanted {
                url: object,
                filename: None,
                sha256: None,
            });
            if out.len() > max {
                return Err(crate::error::limit(
                    "acquire: an S3 listing has more objects than budgets.max_files",
                ));
            }
        }
        let truncated = xml_values(&body, "IsTruncated")
            .first()
            .is_some_and(|v| v == "true");
        token = xml_values(&body, "NextContinuationToken")
            .into_iter()
            .next();
        if !truncated || token.is_none() {
            break;
        }
    }
    Ok(out)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"),
                16,
            )
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Tracked {
    id: JobId,
    wanted: Wanted,
}

/// Runs `acquire@1`.
///
/// # Errors
///
/// See the module documentation.
#[allow(clippy::too_many_lines)]
pub async fn acquire(
    job: &Job,
    env: &Environment,
    secrets: &ResolvedSecrets,
    engines: &AcquireEngine,
    node_instance: &str,
    attempt: u32,
    deadline: tokio::time::Instant,
) -> CoreResult<Vec<OutputFile>> {
    for k in ["pagination", "extract", "filters", "stop"] {
        if job.cfg().contains_key(k) {
            return Err(unsupported(format!(
                "acquire: {k} is page discovery; use crawl@1 and feed its urls to acquire"
            ))
            .into());
        }
    }
    if job.cfg().contains_key("session") || job.config.to_string().contains("infinite_scroll") {
        return Err(config("acquire never holds a session; use zeus.session_acquire").into());
    }
    job.only(&[
        "allowed_domains",
        "sources",
        "listing",
        "headers",
        "query_secrets",
        "fetch",
        "budgets",
        "attribution",
        "allow_partial",
    ])?;
    if !env.network_access {
        return Err(OpError::new(
            "op.network_denied",
            "acquire needs network access, which this environment denies",
        )
        .into());
    }
    let fetch = job.cfg().get("fetch").cloned().unwrap_or(json!({}));
    let fetch = fetch
        .as_object()
        .ok_or_else(|| config("acquire: fetch must be an object"))?;
    if let Some(k) = fetch.keys().find(|k| {
        ![
            "min_interval_ms",
            "max_concurrency_per_host",
            "max_connections_per_file",
            "download_accept",
            "respect_robots",
            "priority",
        ]
        .contains(&k.as_str())
    }) {
        return Err(config(format!("acquire: fetch does not know the member {k}")).into());
    }
    if fetch.get("respect_robots") == Some(&Value::Bool(false)) {
        return Err(config("acquire: respect_robots must be true").into());
    }
    let budgets = job.cfg().get("budgets").cloned().unwrap_or(json!({}));
    let max_files = budgets
        .get("max_files")
        .and_then(Value::as_u64)
        .unwrap_or(10_000)
        .min(1_000_000);
    let max_bytes = budgets.get("max_bytes").and_then(Value::as_u64);
    let allow_partial = job.bool("allow_partial")?.unwrap_or(false);
    let attribution = job.str("attribution")?.map(str::to_owned);

    let mut wanted = wanted_from_sources(job.cfg().get("sources"))?;
    wanted.extend(wanted_from_inputs(job)?);
    if wanted.is_empty() {
        return Err(config("acquire has no URLs: give sources or an input of URLs").into());
    }
    let mut allowed = match allowed_domains(job.cfg().get("allowed_domains"))? {
        Some(a) => a,
        None => AllowedDomains::of_hosts(wanted.iter().filter_map(|w| w.url.host_str()))?,
    };
    if allowed.is_empty() {
        allowed = AllowedDomains::of_hosts(wanted.iter().filter_map(|w| w.url.host_str()))?;
    }
    if let Some(w) = wanted.iter().find(|w| !allowed.allows_url(&w.url)) {
        return Err(OpError::new(
            "op.network_denied",
            format!("{} is outside allowed_domains", redact(w.url.as_str())),
        )
        .into());
    }
    // Named files are not crawled, so requests are not paced unless the configuration asks.
    let pace = if fetch.contains_key("min_interval_ms") {
        min_interval(fetch.get("min_interval_ms"), 200)?
    } else {
        Duration::ZERO
    };
    let mut scope = Scope::new(
        allowed,
        injection(job.cfg(), secrets)?,
        pace,
        env.network_access,
    );
    scope.max_bytes = max_bytes;
    let scope = Arc::new(scope);
    let built = engines.get(env).await?;
    built
        .scopes
        .map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(node_instance.to_owned(), scope.clone());
    let result = run_jobs(
        job,
        &built,
        scope,
        wanted,
        node_instance,
        attempt,
        deadline,
        max_files,
        fetch,
        allow_partial,
        attribution.as_deref(),
    )
    .await;
    built
        .scopes
        .map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(node_instance);
    result
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_jobs(
    job: &Job,
    built: &Built,
    scope: Arc<Scope>,
    mut wanted: Vec<Wanted>,
    node_instance: &str,
    attempt: u32,
    deadline: tokio::time::Instant,
    max_files: u64,
    fetch: &Map<String, Value>,
    allow_partial: bool,
    attribution: Option<&str>,
) -> CoreResult<Vec<OutputFile>> {
    if job.str("listing")? == Some("s3_prefix") {
        let transport = ScopedTransport::new(built.scopes.inner.clone(), scope.clone());
        let mut objects = Vec::new();
        for w in &wanted {
            job.check_cancel()?;
            objects.extend(
                list_s3(
                    &transport,
                    &w.url,
                    usize::try_from(max_files).unwrap_or(usize::MAX),
                )
                .await?,
            );
        }
        job.log(format!(
            "acquire: the listings hold {} objects",
            objects.len()
        ));
        wanted = objects;
    } else if job.str("listing")?.is_some() {
        return Err(config("acquire: listing must be s3_prefix").into());
    }
    if wanted.len() as u64 > max_files {
        return Err(crate::error::limit(format!(
            "acquire: {} URLs exceed budgets.max_files ({max_files})",
            wanted.len()
        ))
        .into());
    }
    let connections = fetch
        .get("max_connections_per_file")
        .and_then(Value::as_u64)
        .map_or(Ok(4u8), |n| {
            u8::try_from(n)
                .ok()
                .filter(|n| (1..=8).contains(n))
                .ok_or_else(|| config("acquire: max_connections_per_file must be 1 to 8"))
        })?;
    let accept = fetch
        .get("download_accept")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let dest = built.downloads.join(node_instance);
    let caller = Caller {
        client_id: CLIENT_ID.into(),
        kind: ActorKind::Engine,
    };
    let engine = &built.engine;
    let mut tracked: Vec<Tracked> = Vec::with_capacity(wanted.len());
    for (i, w) in wanted.into_iter().enumerate() {
        job.check_cancel()?;
        let url = HttpUrl::parse(w.url.as_str())
            .map_err(|_| config("acquire: a URL is not acceptable to the engine"))?;
        let mut source = Source::new(SourceKind::Workflow);
        source.client_id = Some(CLIENT_ID.into());
        let mut spec = JobSpec::new(url, source);
        spec.dest_dir = Some(dest.to_string_lossy().into_owned());
        spec.filename = w
            .filename
            .as_deref()
            .map(|f| files::sanitize_name(f, "download"));
        if let Some(sha) = &w.sha256 {
            spec.expected_sha256 = Some(
                sha.parse()
                    .map_err(|_| config("acquire: a sha256 is not 64 hex digits"))?,
            );
        }
        spec.max_connections = Some(connections);
        spec.transport_hint = Some(zeus_types::TransportHint::Direct);
        if let Some(a) = &accept {
            spec.extra_headers.insert("accept".into(), a.clone());
        }
        spec.labels
            .insert(SCOPE_LABEL.into(), node_instance.to_owned());
        let digest = blake3::hash(w.url.as_str().as_bytes()).to_hex();
        let base_key = format!("ogun:{node_instance}:{i}:{}", &digest[..24]);
        spec.idempotency_key = Some(base_key.clone());
        let outcome = engine
            .enqueue(EnqueueRequest {
                spec: spec.clone(),
                caller: caller.clone(),
            })
            .await
            .map_err(|e| engine_error(&e.to_envelope(), "enqueueing a download"))?;
        let mut id = outcome.job.id;
        if outcome.deduplicated {
            let state = outcome.job.state;
            let file_gone = state == JobState::Completed
                && outcome
                    .job
                    .result
                    .as_ref()
                    .is_none_or(|r| !Path::new(&r.path).exists());
            match state {
                JobState::Failed => {
                    let _ = engine.control_workflow(id, ControlAction::Retry).await;
                }
                JobState::Paused => {
                    let _ = engine.control_workflow(id, ControlAction::Resume).await;
                }
                JobState::Canceled => {}
                _ => {}
            }
            if state == JobState::Canceled || file_gone {
                spec.idempotency_key = Some(format!("{base_key}:a{attempt}"));
                id = engine
                    .enqueue(EnqueueRequest {
                        spec,
                        caller: caller.clone(),
                    })
                    .await
                    .map_err(|e| engine_error(&e.to_envelope(), "enqueueing a download"))?
                    .job
                    .id;
            }
        }
        tracked.push(Tracked { id, wanted: w });
    }
    let total = tracked.len() as u64;
    // Wait until every job is completed, failed or stuck.
    let mut done: Vec<Option<Result<zeus_types::Job, OpError>>> =
        (0..tracked.len()).map(|_| None).collect();
    let mut cancel = job.cancel.clone();
    loop {
        let mut pending = 0u64;
        for (i, t) in tracked.iter().enumerate() {
            if done[i].is_some() {
                continue;
            }
            let snapshot = engine
                .get(t.id)
                .await
                .map_err(|e| engine_error(&e.to_envelope(), "reading a download"))?;
            match snapshot.state {
                JobState::Completed => done[i] = Some(Ok(snapshot)),
                JobState::Failed | JobState::Canceled => {
                    let env = snapshot.error.clone().unwrap_or_else(|| {
                        ErrorEnvelope::from_code(zeus_types::codes::INTERNAL_BUG, "failed")
                    });
                    done[i] = Some(Err(engine_error(
                        &env,
                        &format!("download {} ({})", i + 1, redact(t.wanted.url.as_str())),
                    )));
                }
                JobState::AwaitingApproval | JobState::AwaitingSession => {
                    let _ = engine.control_workflow(t.id, ControlAction::Cancel).await;
                    done[i] = Some(Err(OpError::new(
                        "op.fetch_refused",
                        format!(
                            "download {} would need a session or an approval, which acquire never has",
                            i + 1
                        ),
                    )));
                }
                _ => pending += 1,
            }
        }
        let finished = total - pending;
        job.progress(finished, Some(total));
        if pending == 0 {
            break;
        }
        let interrupted = tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(500)) => None,
            () = tokio::time::sleep_until(deadline) => Some(false),
            r = cancel.changed() => match r {
                Ok(()) if *cancel.borrow() => Some(true),
                _ => None,
            },
        };
        if let Some(cancelled) = interrupted {
            for (i, t) in tracked.iter().enumerate() {
                if done[i].is_none() {
                    let action = if cancelled {
                        ControlAction::Cancel
                    } else {
                        ControlAction::Pause
                    };
                    let _ = engine.control_workflow(t.id, action).await;
                }
            }
            if cancelled {
                return Err(CoreFail::Cancelled);
            }
            return Err(OpError::new("op.timeout", "acquire reached the attempt's timeout; its downloads are paused and resume on the next attempt").into());
        }
    }
    let failures: Vec<&OpError> = done
        .iter()
        .filter_map(|d| d.as_ref().and_then(|r| r.as_ref().err()))
        .collect();
    if !failures.is_empty() {
        job.log(format!(
            "acquire: {} of {total} downloads failed",
            failures.len()
        ));
        if !allow_partial {
            return Err(failures[0].clone().into());
        }
    }
    let out_dir = job.out_dir("artifacts")?;
    let mut outputs = Vec::new();
    for (i, d) in done.into_iter().enumerate() {
        let Some(Ok(snapshot)) = d else { continue };
        let Some(result) = snapshot.result.clone() else {
            continue;
        };
        let src = PathBuf::from(&result.path);
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "download".into());
        let dst = files::unique_path(&out_dir, &name);
        if std::fs::rename(&src, &dst).is_err() {
            std::fs::copy(&src, &dst)
                .map_err(|e| crate::error::io("moving a download into scratch", &e))?;
            let _ = std::fs::remove_file(&src);
        }
        let media_type = snapshot
            .probe
            .as_ref()
            .and_then(|p| p.content_type.clone())
            .map(|c| {
                c.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .filter(|c| c.contains('/') && c != "application/octet-stream")
            .or_else(|| result.detected_mime.clone())
            .unwrap_or_else(|| files::media_type_for(&name).to_owned());
        let url = &tracked[i].wanted.url;
        let mut md = json!({
            "url": redact_record(url.as_str()),
            "source_host": url.host_str().unwrap_or_default(),
            "filename": name,
            "sha256": result.sha256.to_hex(),
            "blake3": result.blake3.to_hex(),
            "size_bytes": result.size_bytes,
        });
        if snapshot.quarantined {
            md["quarantined"] = json!(true);
        }
        if let Some(a) = attribution {
            md["attribution"] = json!(a);
        }
        outputs.push(OutputFile {
            port: "artifacts".into(),
            path: dst,
            media_type,
            metadata: Some(md),
        });
    }
    let _ = std::fs::remove_dir(&dest);
    if outputs.is_empty() && !tracked.is_empty() {
        return Err(internal("acquire completed no download").into());
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_engine_starts_with_no_session_and_scoped_transports() {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment::new(dir.path().to_path_buf());
        let engines = AcquireEngine::default();
        let built = engines.get(&env).await.unwrap();
        assert!(built.downloads.starts_with(dir.path()));
        // A job whose node instance has no active scope gets no transport (fails closed).
        let spec = JobSpec::new(
            HttpUrl::parse("https://data.example.org/a.csv").unwrap(),
            Source::new(SourceKind::Workflow),
        );
        assert!(zeus_engine::JobTransportFactory::for_job(built.scopes.as_ref(), &spec).is_err());
        assert!(!NoSession.extension_connected(None));
        assert!(!NoSession.available(None));
    }

    #[test]
    fn s3_listing_xml() {
        let body = "<ListBucketResult><Contents><Key>records/a&amp;b.csv.gz</Key></Contents><IsTruncated>false</IsTruncated></ListBucketResult>";
        assert_eq!(
            xml_values(body, "Key"),
            vec!["records/a&b.csv.gz".to_owned()]
        );
        assert_eq!(percent_decode("a%3Db/c"), "a=b/c");
    }
}
