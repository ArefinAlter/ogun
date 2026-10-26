//! The daemon's shared state and the documents it keeps in the run store.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ogun_api::auth::IpcSecret;
use ogun_api::methods::{ApprovalView, Diagnostic};
use ogun_api::{ApiError, ApiResult, OgunPaths};
use ogun_ir::{Catalogue, Workflow};
use ogun_ops::Registry;
use ogun_store::{Collection, Store};
use ogun_types::{ApprovalId, IdGenerator, NodeInstanceId, Timestamp, WorkflowId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, watch};

use crate::config::Config;
use crate::creds::Credentials;
use crate::error::{DaemonError, DaemonResult, store_err};
use crate::secrets::SecretStore;
use crate::streams::Streams;

/// A workflow's registry row (`Collection::Workflows`, by name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRow {
    /// Id of the workflow, all versions.
    pub workflow_id: WorkflowId,
    /// Name.
    pub name: String,
    /// Latest version stored.
    pub latest_version: u64,
    /// Its hash.
    pub latest_hash: String,
    /// Triggers fire.
    pub enabled: bool,
    /// First stored.
    pub created_at: Timestamp,
    /// Latest version stored at.
    pub updated_at: Timestamp,
    /// When the scheduler last started a run, Unix ms.
    #[serde(default)]
    pub last_fire_ms: Option<i64>,
    /// The latest version's description.
    #[serde(default)]
    pub description: Option<String>,
}

/// One stored version (`Collection::Versions`, by `name/<20-digit version>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRow {
    /// Name.
    pub name: String,
    /// Version.
    pub version: u64,
    /// Hash.
    pub workflow_hash: String,
    /// When stored.
    pub stored_at: Timestamp,
    /// Approvals this version needs before it runs.
    pub approvals: Vec<ApprovalId>,
    /// Warnings at store time.
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

/// Key of a version row.
#[must_use]
pub fn version_key(name: &str, version: u64) -> String {
    format!("{name}/{version:020}")
}

/// An approval as stored (`Collection::Approvals`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDoc {
    /// What clients see.
    pub view: ApprovalView,
    /// The node instance waiting on it, for run-time approvals.
    #[serde(default)]
    pub instance: Option<NodeInstanceId>,
    /// Code hashes this approval covers (code-change approvals).
    #[serde(default)]
    pub code: Vec<String>,
}

/// One committed output (`Collection::Outputs` holds a list per node instance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRecord {
    /// Output port.
    pub port: String,
    /// Content digest.
    pub blake3: String,
    /// Size.
    pub size_bytes: u64,
    /// Media type.
    pub media_type: String,
    /// Operator metadata (routes read it).
    #[serde(default)]
    pub metadata: Option<Value>,
    /// The route target this item went to, if a route matched.
    #[serde(default)]
    pub route: Option<String>,
    /// Store-local artifact id.
    pub artifact_id: String,
    /// When committed.
    pub committed_at: Timestamp,
}

/// An idempotency record (`Collection::Idempotency`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdempotencyDoc {
    /// BLAKE3 of the JCS form of the parameters.
    pub params_blake3: String,
    /// The first result.
    pub result: Value,
    /// When recorded.
    pub at: Timestamp,
}

/// A retry deadline (`Collection::Retries`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryDoc {
    /// Unix ms after which the back-off has elapsed.
    pub due_ms: i64,
}

/// Executables that count as Ogun trust surfaces.
#[derive(Debug, Clone, Default)]
pub struct Trusted {
    /// The `ogun` CLI (and configured trusted binaries).
    pub cli: Vec<PathBuf>,
    /// Ogun Studio.
    pub studio: Vec<PathBuf>,
}

impl Trusted {
    /// The files next to `ogund`, plus configured ones, canonicalised.
    #[must_use]
    pub fn discover(config: &Config) -> Self {
        let mut me = Self::default();
        let exe_suffix = std::env::consts::EXE_SUFFIX;
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
        {
            for (name, list) in [("ogun", &mut me.cli), ("ogun-studio", &mut me.studio)] {
                if let Ok(p) = dir.join(format!("{name}{exe_suffix}")).canonicalize() {
                    list.push(p);
                }
            }
        }
        for p in &config.trusted_binaries {
            if let Ok(c) = p.canonicalize() {
                me.cli.push(c);
            }
        }
        for p in &config.trusted_studio_binaries {
            if let Ok(c) = p.canonicalize() {
                me.studio.push(c);
            }
        }
        me
    }

    /// Whether `exe` is a pinned CLI.
    #[must_use]
    pub fn is_cli(&self, exe: Option<&Path>) -> bool {
        exe.and_then(|e| e.canonicalize().ok())
            .is_some_and(|e| self.cli.contains(&e))
    }

    /// Whether `exe` is the pinned Studio.
    #[must_use]
    pub fn is_studio(&self, exe: Option<&Path>) -> bool {
        exe.and_then(|e| e.canonicalize().ok())
            .is_some_and(|e| self.studio.contains(&e))
    }
}

/// Everything the daemon shares between connections, the executor, the scheduler and streams.
pub struct Daemon {
    /// Paths.
    pub paths: OgunPaths,
    /// Configuration.
    pub config: Config,
    /// The run store.
    pub store: Store,
    /// The artifact store (Zeus 13).
    pub artifacts: zeus_artifact::ArtifactStore,
    /// Id minting.
    pub ids: IdGenerator,
    /// Operators.
    pub registry: Registry,
    /// The operator catalogue the validator uses.
    pub catalogue: Catalogue,
    /// When started.
    pub started_at: Timestamp,
    /// Local credentials.
    pub creds: Mutex<Credentials>,
    /// The IPC secret.
    pub ipc_secret: IpcSecret,
    /// Secrets for `secret_ref`.
    pub secrets: Arc<SecretStore>,
    /// Wakes the executor.
    pub wake: Notify,
    /// The highest event sequence number; long-polling `events.since` waits on it.
    pub events: watch::Sender<u64>,
    /// Parsed workflows by hash.
    workflows: Mutex<HashMap<String, Arc<Workflow>>>,
    /// Cancel handles of running attempts.
    pub running: Mutex<HashMap<NodeInstanceId, watch::Sender<bool>>>,
    /// Stream sources.
    pub streams: Streams,
    /// Recent decision times, for the per-minute limit.
    pub decisions: Mutex<VecDeque<Instant>>,
    /// Becomes `true` on shutdown.
    pub shutdown: watch::Sender<bool>,
    /// Pinned trust-surface binaries.
    pub trusted: Trusted,
    /// Serializes durable model-budget reservations and settlement.
    pub llm_budget_lock: Mutex<()>,
    /// Chunks of objects edges are uploading (O-11), under `sync/incoming`.
    pub hub: ogun_sync::hub::ChunkAssembler,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("state_dir", &self.paths.state_dir)
            .finish_non_exhaustive()
    }
}

/// Locks a mutex, recovering from poisoning (a panicked holder leaves consistent data here: every
/// guarded structure is rebuilt from the store when in doubt).
pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Daemon {
    /// Opens every store under the state directory and recovers from a crash.
    ///
    /// # Errors
    ///
    /// Any startup failure.
    pub fn open(paths: OgunPaths) -> DaemonResult<Arc<Self>> {
        let config = crate::config::Config::load(&paths.config_file())?;
        let ids = IdGenerator::new();
        let (creds, ipc_secret) = Credentials::load_or_init(&paths, &ids)?;
        for dir in [
            paths.scratch_dir(),
            paths.artifact_dir(),
            paths.state_dir.join("code"),
            paths.state_dir.join("streams"),
        ] {
            crate::creds::private_dir(&dir)?;
        }
        let store = Store::open(&paths.run_store())?;
        let now = Timestamp::now();
        let recovered = store.recover(now)?;
        crate::llm_budget::recover(&store)?;
        if !recovered.is_empty() {
            tracing::info!(
                count = recovered.len(),
                "recovered running node instances after a restart"
            );
        }
        let artifacts = zeus_artifact::ArtifactStore::open(
            &paths.artifact_dir(),
            zeus_artifact::StoreConfig {
                product: "ogun".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                quota_bytes: config.artifact_quota_bytes,
                min_free_bytes: config.min_free_bytes,
                write_roots: vec![],
                ..Default::default()
            },
            Arc::new(zeus_fs::RealFs),
            Arc::new(zeus_control::SystemClock::new()),
            None,
        )
        .map_err(|e| DaemonError::Artifacts(e.to_string()))?;
        let mut env = ogun_ops::Environment::new(paths.state_dir.clone());
        env.operator_dirs.clone_from(&config.operator_dirs);
        env.operator_dirs.push(paths.state_dir.join("operators"));
        // `ogun-op-shim` and `ogun-op-llm` ship next to `ogund`.
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
        {
            env.operator_dirs.push(dir);
        }
        env.model_access = config.model_access;
        env.allow_unsandboxed = config.allow_unsandboxed;
        env.workspace.clone_from(&config.workspace);
        let registry = Registry::new(env);
        let secrets = Arc::new(SecretStore::open(paths.secrets_dir())?);
        let last = store.last_event_seq()?;
        let trusted = Trusted::discover(&config);
        let streams = Streams::new(paths.state_dir.join("streams"));
        let hub =
            ogun_sync::hub::ChunkAssembler::open(&paths.state_dir.join("sync").join("incoming"))
                .map_err(|e| DaemonError::State(format!("sync store: {e}")))?;
        let daemon = Arc::new(Self {
            paths,
            config,
            store,
            artifacts,
            ids,
            registry,
            catalogue: Catalogue::builtin(),
            started_at: now,
            creds: Mutex::new(creds),
            ipc_secret,
            secrets,
            wake: Notify::new(),
            events: watch::Sender::new(last),
            workflows: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            streams,
            decisions: Mutex::new(VecDeque::new()),
            shutdown: watch::Sender::new(false),
            trusted,
            llm_budget_lock: Mutex::new(()),
            hub,
        });
        Ok(daemon)
    }

    /// Publishes the store's event sequence to waiters.
    pub fn bump_events(&self) {
        if let Ok(seq) = self.store.last_event_seq() {
            self.events.send_replace(seq);
        }
    }

    /// Wakes the executor and event waiters after a state change.
    pub fn changed(&self) {
        self.bump_events();
        self.wake.notify_one();
    }

    /// A workflow by hash, parsed once.
    ///
    /// # Errors
    ///
    /// `workflow.not_found` when the store has no such hash.
    pub fn workflow(&self, hash: &str) -> ApiResult<Arc<Workflow>> {
        if let Some(wf) = lock(&self.workflows).get(hash) {
            return Ok(wf.clone());
        }
        let bytes = self
            .store
            .workflow_bytes(hash)
            .map_err(store_err)?
            .ok_or_else(|| ApiError::new("workflow.not_found", "no workflow with that hash"))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ApiError::internal("stored workflow is not UTF-8"))?;
        let wf = Arc::new(
            ogun_ir::parse(text)
                .map_err(|_| ApiError::internal("stored workflow does not parse"))?,
        );
        lock(&self.workflows).insert(hash.to_owned(), wf.clone());
        Ok(wf)
    }

    /// A workflow row by name.
    ///
    /// # Errors
    ///
    /// `workflow.not_found`.
    pub fn workflow_row(&self, name: &str) -> ApiResult<WorkflowRow> {
        self.store
            .get_doc(Collection::Workflows, name)
            .map_err(store_err)?
            .ok_or_else(|| ApiError::new("workflow.not_found", "no workflow by that name"))
    }

    /// A version row; the latest when `version` is `None`.
    ///
    /// # Errors
    ///
    /// `workflow.not_found`.
    pub fn version_row(&self, name: &str, version: Option<u64>) -> ApiResult<VersionRow> {
        let v = match version {
            Some(v) => v,
            None => self.workflow_row(name)?.latest_version,
        };
        self.store
            .get_doc(Collection::Versions, &version_key(name, v))
            .map_err(store_err)?
            .ok_or_else(|| ApiError::new("workflow.not_found", "no such workflow version"))
    }

    /// Whether every approval a version needs is granted.
    #[must_use]
    pub fn runnable(&self, version: &VersionRow) -> bool {
        version.approvals.iter().all(|id| {
            self.store
                .get_doc::<ApprovalDoc>(Collection::Approvals, &id.to_string())
                .ok()
                .flatten()
                .is_some_and(|a| a.view.state == ogun_api::methods::ApprovalState::Approved)
        })
    }

    /// Where a verified code bundle lives.
    #[must_use]
    pub fn code_path(&self, blake3: &str) -> PathBuf {
        self.paths.state_dir.join("code").join(blake3)
    }
}
