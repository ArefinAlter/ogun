//! The edge side of sync (O-11 sections 3 and 6): find sealed window revisions on the edge's own
//! `ogund`, keep them in a durable outbox, offer them to the hub with a signature, upload what the
//! hub wants in chunks, and evict only what the hub acknowledges.
//!
//! The edge always dials out. It talks to two endpoints:
//!
//! * its own `ogund`, over the local IPC endpoint with a `service` credential (default name
//!   `sync`): `workflows.list`, `workflows.get`, `runs.list`, `artifacts.list` and `artifacts.get`
//!   (for the local path of each blob);
//! * the hub's sync bridge over HTTPS: `POST {hub}/api/v1/sync/offer` and
//!   `POST {hub}/api/v1/sync/chunk` (O-11 section 6.2).
//!
//! State lives under `<state_dir>/sync/`: `outbox.json` (pending and acknowledged digests,
//! rewritten atomically) and `acked.jsonl` (one line per acknowledged item, appended, for the
//! edge daemon's ingest-log release, O-10 section 3.4). Resuming an interrupted upload needs no
//! local state: the hub's `want` names the offset it already holds.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ogun_api::methods::{
    ArtifactView, Empty, GetArtifactParams, GetArtifactResult, GetWorkflowParams,
    GetWorkflowResult, ListArtifactsParams, ListArtifactsResult, ListRunsParams, ListRunsResult,
    ListWorkflowsResult, SyncChunkParams, SyncChunkResult, SyncOfferParams, SyncOfferResult, name,
};
use ogun_api::{ApiError, Client, OgunPaths};
use ogun_edge::sync::{Item, Offer, Outbox, Want};
use ogun_types::{EdgeId, OgunError, Timestamp};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::hub::is_partition_key;
use crate::keys::{EdgeKey, KeyError};
use crate::signing::{SignatureError, sign_offer};
use crate::{CHUNK_BYTES, MAX_OFFER_ITEMS, chunk_count};

/// The largest hub response the edge reads.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// The edge's sync configuration, `<state_dir>/sync.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    /// The hub's sync bridge, `https://...` (plain `http://` only for a loopback host).
    pub hub_url: String,
    /// This edge's id, given by the hub when a person enrolled the public key.
    pub edge_id: String,
    /// The Ed25519 key file; `<state_dir>/sync/edge.key` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,
    /// Extra trust roots for the hub (PEM bundle), added to the platform's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<PathBuf>,
    /// Client certificate chain and private key (PEM, `0600`) for mutual TLS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_identity_file: Option<PathBuf>,
    /// The local `ogund` credential name (kind `service`).
    #[serde(default = "SyncConfig::default_credential")]
    pub credential: String,
    /// Workflows to sync; every workflow with window partitions when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows: Vec<String>,
    /// Per workflow, the `node.output` pairs whose artifacts are synced; the stream source's
    /// `window` output when a workflow is not listed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, Vec<String>>,
    /// Pause between sync rounds of `ogun-sync run`.
    #[serde(default = "SyncConfig::default_interval_ms")]
    pub interval_ms: u64,
    /// Timeout of one HTTPS request.
    #[serde(default = "SyncConfig::default_timeout_ms")]
    pub timeout_ms: u64,
    /// Items per offer, at most 1 000.
    #[serde(default = "SyncConfig::default_max_offer_items")]
    pub max_offer_items: usize,
}

impl SyncConfig {
    fn default_credential() -> String {
        "sync".to_owned()
    }
    const fn default_interval_ms() -> u64 {
        60_000
    }
    const fn default_timeout_ms() -> u64 {
        60_000
    }
    const fn default_max_offer_items() -> usize {
        MAX_OFFER_ITEMS
    }

    /// The configuration file for a state directory.
    #[must_use]
    pub fn file(paths: &OgunPaths) -> PathBuf {
        paths.state_dir.join("sync.json")
    }

    /// The default key file for a state directory.
    #[must_use]
    pub fn default_key_file(paths: &OgunPaths) -> PathBuf {
        paths.state_dir.join("sync").join("edge.key")
    }

    /// Reads and checks `sync.json`.
    ///
    /// # Errors
    ///
    /// [`EdgeError::Config`] when the file is missing, malformed, or names an invalid edge id or
    /// hub URL.
    pub fn load(path: &Path) -> Result<Self, EdgeError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| EdgeError::Config(format!("cannot read sync.json: {}", e.kind())))?;
        let config: Self = serde_json::from_str(&text)
            .map_err(|e| EdgeError::Config(format!("sync.json: {e}")))?;
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> Result<(), EdgeError> {
        if self.edge_id.parse::<EdgeId>().is_err() {
            return Err(EdgeError::Config("edge_id is not an edg_ id".into()));
        }
        hub_base(&self.hub_url)?;
        if self.max_offer_items == 0 || self.max_offer_items > MAX_OFFER_ITEMS {
            return Err(EdgeError::Config(
                "max_offer_items must be between 1 and 1000".into(),
            ));
        }
        Ok(())
    }
}

/// Checks the hub URL and returns it without a trailing slash. HTTPS is required except for a
/// loopback host, which tests and single-box setups use.
fn hub_base(url: &str) -> Result<String, EdgeError> {
    let base = url.trim_end_matches('/');
    let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| {
            base.strip_prefix(p).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with(':') || rest.starts_with('/')
            })
        });
    if !(base.starts_with("https://") || loopback) {
        return Err(EdgeError::Config(
            "hub_url must be https:// (http:// only for a loopback host)".into(),
        ));
    }
    if base.contains('@') || base.contains('?') || base.contains('#') {
        return Err(EdgeError::Config(
            "hub_url must not carry credentials, a query or a fragment".into(),
        ));
    }
    Ok(base.to_owned())
}

/// Edge sync failures. Messages never carry key material, tokens or URL queries.
#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    /// Configuration problem.
    #[error("configuration: {0}")]
    Config(String),
    /// The edge key.
    #[error("edge key: {0}")]
    Key(#[from] KeyError),
    /// The local `ogund` refused or is unreachable.
    #[error("local ogund: {0}")]
    Daemon(#[from] ApiError),
    /// The hub answered with an error.
    #[error("hub answered {status}: {error}")]
    Hub {
        /// HTTP status.
        status: u16,
        /// The O-00 envelope from the body (or a synthesized `api.unavailable`).
        error: OgunError,
        /// `Retry-After`, when given.
        retry_after_ms: Option<u64>,
    },
    /// The HTTPS transport failed (connect, TLS, timeout). URLs are stripped.
    #[error("transport: {0}")]
    Transport(String),
    /// A local file failed.
    #[error("local file {what}: {kind:?}")]
    Io {
        /// What was being done.
        what: &'static str,
        /// The I/O error kind.
        kind: std::io::ErrorKind,
    },
    /// The offer could not be signed.
    #[error("signing: {0}")]
    Sign(#[from] SignatureError),
}

impl EdgeError {
    /// How long the hub asked to wait, if it did.
    #[must_use]
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::Hub { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }

    /// The O-00 code, when the error carries one.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Hub { error, .. } => Some(&error.code),
            Self::Daemon(e) => Some(e.code()),
            _ => None,
        }
    }
}

fn io_err(what: &'static str) -> impl Fn(std::io::Error) -> EdgeError {
    move |e| EdgeError::Io {
        what,
        kind: e.kind(),
    }
}

/// What one sync round did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Sealed revisions newly queued in the outbox.
    pub queued: u64,
    /// Offers sent.
    pub offers: u64,
    /// Items the hub acknowledged (now evicted from the outbox).
    pub acked: u64,
    /// Objects fully uploaded and verified by the hub.
    pub uploaded: u64,
    /// Chunks sent.
    pub chunks: u64,
    /// Bytes sent (raw, before base64).
    pub bytes: u64,
    /// Items still pending.
    pub pending: u64,
}

/// The durable part of the edge's sync state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    outbox: Outbox,
}

/// The edge sync client.
#[derive(Debug)]
pub struct EdgeSync {
    config: SyncConfig,
    hub: String,
    key: EdgeKey,
    http: reqwest::Client,
    state_dir: PathBuf,
    state: State,
}

fn read_private(path: &Path, what: &'static str) -> Result<Zeroizing<Vec<u8>>, EdgeError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(io_err(what))?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(EdgeError::Config(format!(
                "{what} is accessible to other users; it must be mode 0600"
            )));
        }
    }
    Ok(Zeroizing::new(std::fs::read(path).map_err(io_err(what))?))
}

fn transport(e: reqwest::Error) -> EdgeError {
    EdgeError::Transport(e.without_url().to_string())
}

/// Whether an upload error concerns only that object (its local file, or the hub refusing that
/// chunk), as opposed to the link, the hub as a whole or the local daemon.
fn item_local(e: &EdgeError) -> bool {
    match e {
        EdgeError::Io { .. } | EdgeError::Config(_) => true,
        EdgeError::Hub { status, error, .. } => {
            *status != 429 && *status < 500 && error.code != "edge.not_enrolled"
        }
        EdgeError::Daemon(d) => d.code() == "artifact.not_found",
        _ => false,
    }
}

impl EdgeSync {
    /// Builds the client: loads the key, prepares TLS and reads the outbox.
    ///
    /// # Errors
    ///
    /// [`EdgeError`] for configuration, key, TLS material or state file problems.
    pub fn new(config: SyncConfig, paths: &OgunPaths) -> Result<Self, EdgeError> {
        config.check()?;
        let hub = hub_base(&config.hub_url)?;
        let key_file = config
            .key_file
            .clone()
            .unwrap_or_else(|| SyncConfig::default_key_file(paths));
        let key = EdgeKey::load(&key_file)?;
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms.max(1_000)))
            .connect_timeout(Duration::from_secs(15))
            .user_agent(concat!("ogun-sync/", env!("CARGO_PKG_VERSION")))
            .https_only(hub.starts_with("https://"));
        if let Some(ca) = &config.ca_file {
            let pem = std::fs::read(ca).map_err(io_err("ca_file"))?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|_| EdgeError::Config("ca_file is not a PEM bundle".into()))?;
            builder = builder.tls_certs_merge(certs);
        }
        if let Some(id) = &config.client_identity_file {
            let pem = read_private(id, "client_identity_file")?;
            let identity = reqwest::Identity::from_pem(&pem).map_err(|_| {
                EdgeError::Config(
                    "client_identity_file needs a PEM certificate chain and private key".into(),
                )
            })?;
            builder = builder.identity(identity);
        }
        let http = builder.build().map_err(transport)?;
        let state_dir = paths.state_dir.join("sync");
        crate::keys::create_private_dir(&state_dir)?;
        let state = match std::fs::read(state_dir.join("outbox.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| EdgeError::Config("sync/outbox.json is corrupt".into()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(io_err("outbox.json")(e)),
        };
        Ok(Self {
            config,
            hub,
            key,
            http,
            state_dir,
            state,
        })
    }

    /// The hub URL with credentials and query stripped, for logs.
    #[must_use]
    pub fn hub_for_logs(&self) -> String {
        zeus_types::redact_url(&self.hub, zeus_types::RedactMode::Log).url
    }

    /// Items waiting for the hub.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.state.outbox.pending()
    }

    fn save(&self) -> Result<(), EdgeError> {
        let bytes = serde_json::to_vec(&self.state)
            .map_err(|_| EdgeError::Config("cannot serialise the outbox".into()))?;
        let path = self.state_dir.join("outbox.json");
        let tmp = self.state_dir.join("outbox.json.tmp");
        let mut f = std::fs::File::create(&tmp).map_err(io_err("outbox.json"))?;
        f.write_all(&bytes)
            .and_then(|()| f.sync_all())
            .map_err(io_err("outbox.json"))?;
        std::fs::rename(&tmp, &path).map_err(io_err("outbox.json"))
    }

    fn record_acked(&self, items: &[Item]) -> Result<(), EdgeError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state_dir.join("acked.jsonl"))
            .map_err(io_err("acked.jsonl"))?;
        let at = Timestamp::now().to_rfc3339();
        let mut out = Vec::new();
        for item in items {
            let line = serde_json::json!({
                "asset": item.asset,
                "partition": item.partition,
                "revision": item.revision,
                "blake3": item.blake3,
                "size_bytes": item.size_bytes,
                "acked_at": at,
            });
            out.extend_from_slice(line.to_string().as_bytes());
            out.push(b'\n');
        }
        f.write_all(&out)
            .and_then(|()| f.sync_all())
            .map_err(io_err("acked.jsonl"))
    }

    fn asset(&self, workflow: &str, node: &str, output: &str) -> String {
        format!("{}/{workflow}/{node}.{output}", self.config.edge_id)
    }

    /// The `node.output` pairs to sync for a workflow, or `None` when it has no window partitions
    /// and is not configured.
    fn outputs_for(
        &self,
        workflow: &str,
        doc: &serde_json::Value,
    ) -> Option<Vec<(String, String)>> {
        let configured = self.config.outputs.get(workflow).map(|list| {
            list.iter()
                .filter_map(|s| s.split_once('.'))
                .map(|(n, o)| (n.to_owned(), o.to_owned()))
                .collect::<Vec<_>>()
        });
        if configured.is_some() {
            return configured;
        }
        let wf: ogun_ir::Workflow = serde_json::from_value(doc.clone()).ok()?;
        match wf.partitions {
            Some(ogun_ir::model::Partitions::Window(w)) => {
                Some(vec![(w.source, "window".to_owned())])
            }
            _ => None,
        }
    }

    /// Finds sealed revisions on the local `ogund` and queues the new ones. Returns how many were
    /// queued. A revision number is the position of a digest among the distinct digests committed
    /// for the same node, output and partition, in commit order, from 1.
    ///
    /// # Errors
    ///
    /// [`EdgeError::Daemon`] or a state file error.
    pub async fn collect(&mut self, ogund: &mut Client) -> Result<u64, EdgeError> {
        let list: ListWorkflowsResult = ogund.call(name::WORKFLOWS_LIST, &Empty {}).await?;
        let mut queued = 0;
        for summary in list.workflows {
            if !self.config.workflows.is_empty() && !self.config.workflows.contains(&summary.name) {
                continue;
            }
            let got: GetWorkflowResult = ogund
                .call(
                    name::WORKFLOWS_GET,
                    &GetWorkflowParams {
                        name: summary.name.clone(),
                        version: None,
                    },
                )
                .await?;
            let Some(outputs) = self.outputs_for(&summary.name, &got.workflow) else {
                continue;
            };
            let mut artifacts: Vec<ArtifactView> = Vec::new();
            let mut before = None;
            loop {
                let page: ListRunsResult = ogund
                    .call(
                        name::RUNS_LIST,
                        &ListRunsParams {
                            workflow: Some(summary.name.clone()),
                            before,
                            limit: Some(500),
                        },
                    )
                    .await?;
                for run in &page.runs {
                    let listed: ListArtifactsResult = ogund
                        .call(
                            name::ARTIFACTS_LIST,
                            &ListArtifactsParams { run_id: run.run_id },
                        )
                        .await?;
                    // Every run is listed on every round: `artifacts.list` has no cursor yet
                    // (O-11 open issue 4).
                    artifacts.extend(listed.artifacts.into_iter().filter(|a| {
                        is_partition_key(&a.partition)
                            && outputs.iter().any(|(n, o)| *n == a.node && *o == a.output)
                    }));
                }
                match page.next {
                    Some(next) => before = Some(next),
                    None => break,
                }
            }
            artifacts.sort_by(|a, b| {
                (&a.node, &a.output, &a.partition, a.committed_at).cmp(&(
                    &b.node,
                    &b.output,
                    &b.partition,
                    b.committed_at,
                ))
            });
            let mut seen: BTreeMap<(String, String, String), Vec<String>> = BTreeMap::new();
            for a in artifacts {
                let digests = seen
                    .entry((a.node.clone(), a.output.clone(), a.partition.clone()))
                    .or_default();
                if digests.contains(&a.blake3) {
                    continue;
                }
                digests.push(a.blake3.clone());
                let revision = u32::try_from(digests.len()).unwrap_or(u32::MAX);
                let before = self.state.outbox.pending();
                self.state.outbox.push(Item {
                    asset: self.asset(&summary.name, &a.node, &a.output),
                    partition: a.partition,
                    revision,
                    blake3: a.blake3,
                    size_bytes: a.size_bytes,
                });
                if self.state.outbox.pending() > before {
                    queued += 1;
                }
            }
        }
        if queued > 0 {
            self.save()?;
        }
        Ok(queued)
    }

    async fn post<P: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        route: &str,
        body: &P,
    ) -> Result<R, EdgeError> {
        let url = format!("{}/api/v1/sync/{route}", self.hub);
        let mut resp = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        let retry_after_ms = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|s| s.saturating_mul(1_000));
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(transport)? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(EdgeError::Transport("hub response too large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        if status.is_success() {
            return serde_json::from_slice(&bytes)
                .map_err(|_| EdgeError::Transport(format!("unexpected {route} response")));
        }
        #[derive(Deserialize)]
        struct Body {
            error: OgunError,
        }
        let error = serde_json::from_slice::<Body>(&bytes).map_or_else(
            |_| OgunError::new("api.unavailable", format!("hub returned HTTP {status}")),
            |b| b.error,
        );
        Err(EdgeError::Hub {
            status: status.as_u16(),
            error,
            retry_after_ms,
        })
    }

    async fn offer(&self, offer: &Offer) -> Result<Want, EdgeError> {
        let (value, signature) = sign_offer(&self.key, offer)?;
        let params = SyncOfferParams {
            edge_id: offer
                .edge_id
                .parse()
                .map_err(|_| EdgeError::Config("edge_id is not an edg_ id".into()))?,
            offer: value,
            signature,
        };
        let result: SyncOfferResult = self.post("offer", &params).await?;
        serde_json::from_value(result.want)
            .map_err(|_| EdgeError::Transport("the hub's want is malformed".into()))
    }

    /// Uploads one wanted object from `offset`. Returns (chunks sent, bytes sent, complete).
    async fn upload(
        &self,
        ogund: &mut Client,
        item: &Item,
        offset: u64,
    ) -> Result<(u64, u64, bool), EdgeError> {
        let got: GetArtifactResult = ogund
            .call(
                name::ARTIFACTS_GET,
                &GetArtifactParams {
                    blake3: item.blake3.clone(),
                    inline_max_bytes: 0,
                },
            )
            .await?;
        let mut file = std::fs::File::open(&got.path).map_err(io_err("artifact"))?;
        let size = file.metadata().map_err(io_err("artifact"))?.len();
        if size != item.size_bytes {
            return Err(EdgeError::Config(format!(
                "local artifact {} has {size} bytes, the offer says {}",
                item.blake3, item.size_bytes
            )));
        }
        let total = chunk_count(size);
        let first = (offset / CHUNK_BYTES).min(total - 1);
        let edge_id = self
            .config
            .edge_id
            .parse()
            .map_err(|_| EdgeError::Config("edge_id is not an edg_ id".into()))?;
        let mut sent = (0, 0, false);
        for index in first..total {
            let start = index * CHUNK_BYTES;
            let len = CHUNK_BYTES.min(size - start);
            let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
            file.seek(SeekFrom::Start(start))
                .and_then(|_| file.read_exact(&mut buf))
                .map_err(io_err("artifact"))?;
            let params = SyncChunkParams {
                edge_id,
                partition: item.partition.clone(),
                blake3: item.blake3.clone(),
                index: u32::try_from(index).unwrap_or(u32::MAX),
                total: u32::try_from(total).unwrap_or(u32::MAX),
                data_base64: STANDARD.encode(&buf),
            };
            let result: SyncChunkResult = self.post("chunk", &params).await?;
            sent.0 += 1;
            sent.1 += len;
            sent.2 = result.complete;
        }
        Ok(sent)
    }

    /// One sync round: collect, then offer and upload until the hub has acknowledged everything
    /// or no further progress is made.
    ///
    /// # Errors
    ///
    /// The first error that stops the round. Progress made before it (acknowledgements, complete
    /// uploads) is kept.
    pub async fn once(&mut self, paths: &OgunPaths) -> Result<Report, EdgeError> {
        let mut ogund = Client::connect(
            paths,
            &self.config.credential,
            ogun_api::methods::ClientInfo {
                name: "ogun-sync".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await?;
        let mut report = Report {
            queued: self.collect(&mut ogund).await?,
            ..Report::default()
        };
        let mut idle_rounds = 0;
        while self.state.outbox.pending() > 0 && idle_rounds < 2 {
            let mut offer = self.state.outbox.offer(&self.config.edge_id);
            offer.items.truncate(self.config.max_offer_items);
            let want = self.offer(&offer).await?;
            report.offers += 1;
            let evicted = self.state.outbox.ack(&want.acked);
            if !evicted.is_empty() {
                self.record_acked(&evicted)?;
                self.save()?;
            }
            report.acked += evicted.len() as u64;
            let mut uploaded = 0;
            let by_digest: BTreeMap<&str, &Item> =
                offer.items.iter().map(|i| (i.blake3.as_str(), i)).collect();
            for (digest, offset) in &want.blobs {
                let Some(item) = by_digest.get(digest.as_str()) else {
                    continue;
                };
                let (chunks, bytes, complete) = match self.upload(&mut ogund, item, *offset).await {
                    Ok(v) => v,
                    // One object's problem (its local file, a refused or mismatched upload)
                    // must not block the others; it is retried next round.
                    Err(e) if item_local(&e) => {
                        tracing::warn!(blake3 = %item.blake3, partition = %item.partition, error = %e, "upload of one object failed");
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                report.chunks += chunks;
                report.bytes += bytes;
                if complete {
                    uploaded += 1;
                }
            }
            report.uploaded += uploaded;
            if evicted.is_empty() && uploaded == 0 {
                idle_rounds += 1;
            } else {
                idle_rounds = 0;
            }
        }
        report.pending = self.state.outbox.pending() as u64;
        Ok(report)
    }

    /// Runs rounds forever, `interval_ms` apart (longer when the hub asks for it).
    pub async fn run(&mut self, paths: &OgunPaths) {
        loop {
            let mut wait = self.config.interval_ms;
            match self.once(paths).await {
                Ok(r) => tracing::info!(
                    queued = r.queued,
                    offers = r.offers,
                    acked = r.acked,
                    uploaded = r.uploaded,
                    chunks = r.chunks,
                    bytes = r.bytes,
                    pending = r.pending,
                    "sync round done"
                ),
                Err(e) => {
                    if e.code() == Some("edge.not_enrolled") {
                        tracing::error!(
                            public_key = %self.key.public_key(),
                            "the hub does not know this edge; a person must enrol its public key on the hub"
                        );
                    } else {
                        tracing::warn!(error = %e, pending = self.pending(), "sync round failed");
                    }
                    if let Some(ms) = e.retry_after_ms() {
                        wait = wait.max(ms);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_urls_must_be_https_or_loopback() {
        assert!(hub_base("https://hub.example.org/").is_ok());
        assert!(hub_base("http://127.0.0.1:8443").is_ok());
        assert!(hub_base("http://localhost").is_ok());
        assert!(hub_base("http://hub.example.org").is_err());
        assert!(hub_base("http://localhost.evil.example").is_err());
        assert!(hub_base("https://user:pw@hub.example.org").is_err());
        assert!(hub_base("https://hub.example.org/?token=x").is_err());
    }

    #[test]
    fn config_defaults() {
        let c: SyncConfig = serde_json::from_str(
            r#"{"hub_url": "https://hub.example.org", "edge_id": "edg_01M45MN5M0PZ0D9W0APDCGRNZD"}"#,
        )
        .unwrap();
        c.check().unwrap();
        assert_eq!(c.credential, "sync");
        assert_eq!(c.interval_ms, 60_000);
        assert_eq!(c.max_offer_items, 1_000);
    }
}
