//! What `ogund` uses: connect, register the import root, submit session-backed jobs, follow them and import their
//! files (O-08 sections 5 to 8).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use zeus_artifact::{
    ArtifactDraft, ArtifactOrigin, ArtifactStore, Operation, Retention, VerificationBasis,
};
use zeus_types::job::ALLOWED_EXTRA_HEADERS;
use zeus_types::{HttpUrl, JobId, JobState, Origin, QuarantineReason, RedactedUrl};
use zeus_verify::hash::Digests;
use zeus_verify::provenance::ManifestHttp;

use crate::client::{Session, ZeusClient};
use crate::config::BridgeConfig;
use crate::job::{JobStatus, JobView, ZeusProvenance};
use crate::{BridgeError, BridgeResult};

/// Members an operator's session request may carry (O-08 section 6). Everything else is refused, so a request
/// can never name a destination, a cookie, an `Authorization` header or an overwrite of a user's file.
pub const REQUEST_MEMBERS: &[&str] = &[
    "url",
    "hint_url",
    "referrer",
    "filename",
    "expected_sha256",
    "expected_size",
    "priority",
    "session_origin",
    "grant_scope_hint",
    "extra_headers",
    "overwrite",
    "max_connections",
    "transport_hint",
    "labels",
];

/// Longest `jobs.wait` Zeus allows.
pub const MAX_WAIT_MS: u64 = 60_000;

/// How the daemon wants a job submitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitOptions {
    /// Idempotency key (Zeus 05 section 12). `ogund` uses `<node instance>:<attempt>`, so a resubmission after a
    /// restart returns the same job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// A subdirectory of the import root for this job's file (for example the run id). Relative, plain names only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// Labels the daemon adds (`ogun.run`, `ogun.node_instance`); they win over the operator's labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// A submitted Zeus job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submitted {
    /// `job_...`.
    pub job_id: String,
    /// Its state right after creation (`queued`, or `awaiting_approval` when a person must grant the session).
    pub state: JobState,
    /// The Zeus approval a person must decide on a Zeus trust surface, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    /// The call was answered from Zeus's idempotency store.
    pub replayed: bool,
}

/// A registered root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRoot {
    /// The canonical path.
    pub path: PathBuf,
    /// Zeus created the record now (false when it already existed).
    pub created: bool,
}

/// What a person sees about a Zeus approval Ogun waits for. Read-only: Ogun cannot decide it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeusApprovalView {
    /// `ap_...`.
    pub id: String,
    /// Kind (`session_grant`).
    pub kind: String,
    /// State (`pending`, `approved`, `denied`, `expired`, `cancelled`).
    pub state: String,
    /// The origin whose session is asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// When it expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// A completed job's file, verified to lie in Ogun's root, with everything needed to import it.
#[derive(Debug, Clone)]
pub struct PendingImport {
    /// The Zeus job.
    pub job_id: String,
    /// The file Zeus wrote.
    pub source_path: PathBuf,
    /// Zeus's export form of the artifact record (Zeus 13 section 10.1), when the daemon offers `artifacts.*`.
    pub export_form: Option<Value>,
    /// A record built from the job view, used when there is no export form.
    pub draft: ArtifactDraft,
    /// The digests Zeus reported; the store recomputes them from the bytes and refuses a mismatch.
    pub expected: Digests,
    /// What Zeus reported about the download.
    pub provenance: ZeusProvenance,
}

/// What Ogun's store committed: the plain inputs the daemon records for the node instance's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedArtifact {
    /// Ogun's artifact id (`art_...`, local to Ogun's store).
    pub artifact_id: String,
    /// BLAKE3: the identity across both stores.
    pub blake3: String,
    /// SHA-256.
    pub sha256: String,
    /// Size.
    pub size_bytes: u64,
    /// Media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// The quarantine flag Zeus set survives the import.
    pub quarantined: bool,
    /// The Zeus job.
    pub zeus_job_id: String,
    /// Zeus's artifact id, when imported from the export form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zeus_artifact_id: Option<String>,
    /// `export_form` (Zeus 13 import with an import hop) or `job_view` (ingest with a Zeus-origin record).
    pub via: String,
    /// The object in Ogun's store (read-only).
    pub object_path: PathBuf,
}

impl PendingImport {
    /// Commits the file into Ogun's artifact store, with `keep` retention (Zeus 13 section 9.1: Ogun keeps what it
    /// imports). With an export form this is a Zeus 13 section 10.2 import, which records the hop and the
    /// received record's hash; otherwise an ingest with the expected digests and a Zeus-origin record.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Artifact`] with the store's code (`artifact.digest_mismatch` when the bytes changed).
    pub fn commit(self, store: &ArtifactStore) -> BridgeResult<ImportedArtifact> {
        let zeus_artifact_id = self
            .export_form
            .as_ref()
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let (artifact, via) = match &self.export_form {
            Some(s) => (
                store.import(s, Some(&self.source_path), "zeus", "api", Retention::Keep),
                "export_form",
            ),
            None => {
                let key = format!("zeus:{}", self.job_id);
                (
                    store.ingest_expected(
                        &self.source_path,
                        self.draft,
                        Some(&key),
                        Some(self.expected),
                    ),
                    "job_view",
                )
            }
        };
        let artifact = artifact.map_err(|e| BridgeError::Artifact(e.to_string()))?;
        Ok(ImportedArtifact {
            artifact_id: artifact.id.to_string(),
            blake3: artifact.blake3.to_string(),
            sha256: artifact.sha256.to_string(),
            size_bytes: artifact.size_bytes,
            media_type: artifact
                .media_type
                .clone()
                .or(artifact.declared_media_type.clone()),
            quarantined: artifact.quarantined,
            zeus_job_id: self.job_id,
            zeus_artifact_id,
            via: via.to_owned(),
            object_path: store.object_path(artifact.blake3),
        })
    }
}

/// The session bridge: one authenticated `service` connection to `zeusd`, reconnected when it breaks.
#[derive(Debug)]
pub struct ZeusBridge {
    config: BridgeConfig,
    client: Mutex<Option<ZeusClient>>,
    roots: std::sync::Mutex<Vec<PathBuf>>,
}

fn invalid(msg: impl Into<String>) -> BridgeError {
    BridgeError::InvalidRequest(msg.into())
}

fn string_member<'a>(obj: &'a Map<String, Value>, key: &str) -> BridgeResult<Option<&'a str>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(invalid(format!("{key} must be a string"))),
    }
}

fn string_map(value: Option<&Value>, key: &str) -> BridgeResult<BTreeMap<String, String>> {
    match value {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(m)) => m
            .iter()
            .map(|(k, v)| match v {
                Value::String(s) => Ok((k.clone(), s.clone())),
                _ => Err(invalid(format!("{key} values must be strings"))),
            })
            .collect(),
        Some(_) => Err(invalid(format!("{key} must be an object"))),
    }
}

/// Checks a subdirectory name: relative, at most 8 components of `[A-Za-z0-9._-]`, no `.` or `..`.
fn safe_subdir(text: &str) -> BridgeResult<PathBuf> {
    let p = Path::new(text);
    let mut n = 0;
    for c in p.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().ok_or_else(|| invalid("subdir is not UTF-8"))?;
                if s.is_empty()
                    || s.len() > 64
                    || s.starts_with('.')
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                {
                    return Err(invalid("subdir components must be plain names"));
                }
                n += 1;
            }
            _ => return Err(invalid("subdir must be a relative path of plain names")),
        }
    }
    if n == 0 || n > 8 {
        return Err(invalid("subdir must have 1 to 8 components"));
    }
    Ok(p.to_path_buf())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Builds the `NewJob` for `jobs.enqueue` from an operator's request (O-08 section 6).
///
/// # Errors
///
/// [`BridgeError::InvalidRequest`]; the message names members, never values.
pub fn new_job(
    request: &Value,
    dest_dir: &Path,
    extra_labels: &BTreeMap<String, String>,
) -> BridgeResult<Value> {
    let obj = request
        .as_object()
        .ok_or_else(|| invalid("the session request must be a JSON object"))?;
    if let Some(k) = obj.keys().find(|k| !REQUEST_MEMBERS.contains(&k.as_str())) {
        return Err(invalid(format!(
            "member {k} is not allowed in a session request"
        )));
    }
    let url = string_member(obj, "url")?.ok_or_else(|| invalid("url is required"))?;
    HttpUrl::parse(url).map_err(|_| invalid("url is not an absolute http(s) URL"))?;
    for key in ["hint_url", "referrer"] {
        if let Some(u) = string_member(obj, key)? {
            HttpUrl::parse(u)
                .map_err(|_| invalid(format!("{key} is not an absolute http(s) URL")))?;
        }
    }
    let origin = string_member(obj, "session_origin")?.ok_or_else(|| {
        invalid("session_origin is required: the bridge carries only session-backed jobs")
    })?;
    let parsed = Origin::parse(origin).map_err(|_| invalid("session_origin is not an origin"))?;
    if !parsed.allows_session_release() {
        return Err(invalid("session_origin must be https (or loopback http)"));
    }
    if let Some(o) = string_member(obj, "overwrite")?
        && !matches!(o, "rename" | "fail")
    {
        return Err(invalid(
            "overwrite must be rename or fail; the bridge never replaces a file",
        ));
    }
    let headers = string_map(obj.get("extra_headers"), "extra_headers")?;
    if headers
        .keys()
        .any(|k| !ALLOWED_EXTRA_HEADERS.contains(&k.to_ascii_lowercase().as_str()))
    {
        return Err(invalid(
            "extra_headers names a header outside Zeus's allow-list",
        ));
    }
    let mut labels = string_map(obj.get("labels"), "labels")?;
    labels.extend(extra_labels.iter().map(|(k, v)| (k.clone(), v.clone())));
    if labels.len() > zeus_types::limits::MAX_LABELS_PER_JOB {
        return Err(invalid("too many labels"));
    }
    let mut job = obj.clone();
    job.insert(
        "dest_dir".into(),
        Value::String(
            dest_dir
                .to_str()
                .ok_or_else(|| invalid("the import root is not UTF-8"))?
                .to_owned(),
        ),
    );
    if labels.is_empty() {
        job.remove("labels");
    } else {
        job.insert(
            "labels".into(),
            serde_json::to_value(labels).unwrap_or(Value::Null),
        );
    }
    Ok(Value::Object(job))
}

impl ZeusBridge {
    /// Connects to `zeusd` with the configured service credential.
    ///
    /// # Errors
    ///
    /// As [`ZeusClient::connect`].
    pub async fn connect(config: BridgeConfig) -> BridgeResult<Self> {
        let client = ZeusClient::connect(&config).await?;
        Ok(Self {
            config,
            client: Mutex::new(Some(client)),
            roots: std::sync::Mutex::new(vec![]),
        })
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    /// The current session, when connected.
    pub async fn session(&self) -> Option<Session> {
        self.client
            .lock()
            .await
            .as_ref()
            .map(|c| c.session().clone())
    }

    async fn call(&self, method: &str, params: Value, extra: Duration) -> BridgeResult<Value> {
        let mut guard = self.client.lock().await;
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(ZeusClient::connect(&self.config).await?);
            }
            let Some(client) = guard.as_mut() else {
                continue;
            };
            match client.call_with_extra(method, params.clone(), extra).await {
                // A broken connection is dropped and, once, re-established. Mutating calls carry idempotency keys,
                // so repeating one cannot do the work twice.
                Err(BridgeError::Unavailable(reason)) if attempt == 0 => {
                    tracing::info!(%method, %reason, "zeusd connection lost; reconnecting");
                    *guard = None;
                }
                other => return other,
            }
        }
        Err(BridgeError::Unavailable("zeusd connection lost".into()))
    }

    async fn has_capability(&self, capability: &str) -> bool {
        self.client
            .lock()
            .await
            .as_ref()
            .is_some_and(|c| c.has_capability(capability))
    }

    fn known_roots(&self) -> Vec<PathBuf> {
        let mut roots = self.roots.lock().map(|r| r.clone()).unwrap_or_default();
        if let Ok(own) = std::fs::canonicalize(&self.config.import_root)
            && !roots.contains(&own)
        {
            roots.push(own);
        }
        roots
    }

    /// Registers a directory Ogun owns as one of its Zeus write roots (`files.roots.register`, Zeus 05 section 9.6).
    /// Idempotent: registering the same path again returns the existing record.
    ///
    /// # Errors
    ///
    /// [`BridgeError::InvalidRequest`] for a relative path; Zeus's `policy.path_outside_roots` when the path is not
    /// inside one of the user's Zeus write roots (a person sets those in Zeus).
    pub async fn register_root(&self, path: &Path) -> BridgeResult<RegisteredRoot> {
        if !path.is_absolute() {
            return Err(invalid("a root must be an absolute path"));
        }
        let text = path
            .to_str()
            .ok_or_else(|| invalid("a root must be UTF-8"))?;
        let key = format!(
            "ogun-root-{}",
            &blake3::hash(text.as_bytes()).to_hex()[..32]
        );
        let result = self
            .call(
                "files.roots.register",
                json!({ "path": text, "label": self.config.root_label, "idempotency_key": key }),
                Duration::ZERO,
            )
            .await?;
        let registered = result
            .get("root")
            .and_then(|r| r.get("path"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BridgeError::Protocol("files.roots.register result has no root path".into())
            })?;
        let canonical =
            std::fs::canonicalize(registered).unwrap_or_else(|_| PathBuf::from(registered));
        if let Ok(mut roots) = self.roots.lock()
            && !roots.contains(&canonical)
        {
            roots.push(canonical.clone());
        }
        Ok(RegisteredRoot {
            path: canonical,
            created: result
                .get("created")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// Registers the configured import root.
    ///
    /// # Errors
    ///
    /// As [`ZeusBridge::register_root`].
    pub async fn register_import_root(&self) -> BridgeResult<RegisteredRoot> {
        let root = self.config.import_root.clone();
        self.register_root(&root).await
    }

    /// Submits the request an operator returned as `Outcome::SessionRequired`.
    ///
    /// # Errors
    ///
    /// As [`ZeusBridge::submit_with`].
    pub async fn submit(&self, request: &Value) -> BridgeResult<Submitted> {
        self.submit_with(request, &SubmitOptions::default()).await
    }

    /// Submits one session-backed Zeus job. The destination is always inside the first registered root (and
    /// `subdir` below it); a job that needs a session Zeus has not granted is created in `awaiting_approval`, and
    /// only a person on a Zeus trust surface can approve it.
    ///
    /// # Errors
    ///
    /// [`BridgeError::InvalidRequest`] for a request outside O-08 section 6; [`BridgeError::PathOutsideRoot`] when
    /// no root is registered yet; Zeus's errors (`policy.*`, `api.rate_limited`, ...) as [`BridgeError::Zeus`].
    pub async fn submit_with(
        &self,
        request: &Value,
        opts: &SubmitOptions,
    ) -> BridgeResult<Submitted> {
        let root = self
            .roots
            .lock()
            .ok()
            .and_then(|r| r.first().cloned())
            .ok_or(BridgeError::PathOutsideRoot)?;
        let dest = match &opts.subdir {
            Some(s) => root.join(safe_subdir(s)?),
            None => root,
        };
        create_private_dir(&dest)?;
        let job = new_job(request, &dest, &opts.labels)?;
        let mut params = json!({ "jobs": [job] });
        if let Some(k) = &opts.idempotency_key {
            params["idempotency_key"] = Value::String(k.clone());
        }
        let result = self.call("jobs.enqueue", params, Duration::ZERO).await?;
        let replayed = result
            .get("replayed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let entry = result
            .get("jobs")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| BridgeError::Protocol("jobs.enqueue returned no entry".into()))?;
        if let Some(err) = entry.get("error") {
            return Err(serde_json::from_value(err.clone())
                .map(|e| BridgeError::Zeus(Box::new(e)))
                .unwrap_or_else(|_| {
                    BridgeError::Protocol("jobs.enqueue entry error has the wrong shape".into())
                }));
        }
        let job_id = entry
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or_else(|| BridgeError::Protocol("jobs.enqueue entry has no job_id".into()))?
            .to_owned();
        let state: JobState = entry
            .get("state")
            .cloned()
            .and_then(|s| serde_json::from_value(s).ok())
            .ok_or_else(|| BridgeError::Protocol("jobs.enqueue entry has no state".into()))?;
        let approval_id = entry
            .get("approval_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        tracing::info!(%job_id, ?state, replayed, "session-backed job submitted to zeusd");
        Ok(Submitted {
            job_id,
            state,
            approval_id,
            replayed,
        })
    }

    async fn job_view(&self, job_id: &str) -> BridgeResult<JobView> {
        let result = self
            .call("jobs.get", json!({ "job_id": job_id }), Duration::ZERO)
            .await?;
        serde_json::from_value(result.get("job").cloned().unwrap_or(Value::Null))
            .map_err(|_| BridgeError::Protocol("jobs.get result has the wrong shape".into()))
    }

    /// The job's status.
    ///
    /// # Errors
    ///
    /// Zeus's `api.not_found` (also for jobs of other clients) or transport failures.
    pub async fn status(&self, job_id: &str) -> BridgeResult<JobStatus> {
        Ok(self.job_view(job_id).await?.status())
    }

    /// Waits up to `timeout_ms` (at most 60 s) for the job to finish or block, then returns its status.
    ///
    /// # Errors
    ///
    /// As [`ZeusBridge::status`].
    pub async fn wait(&self, job_id: &str, timeout_ms: u64) -> BridgeResult<JobStatus> {
        let timeout_ms = timeout_ms.clamp(1, MAX_WAIT_MS);
        self.call(
            "jobs.wait",
            json!({ "job_ids": [job_id], "until": "settled_or_blocked", "timeout_ms": timeout_ms }),
            Duration::from_millis(timeout_ms + 5_000),
        )
        .await?;
        self.status(job_id).await
    }

    /// Cancels a job (a run or node of Ogun's was cancelled).
    ///
    /// # Errors
    ///
    /// Zeus's errors, for example `api.invalid_transition` for a finished job.
    pub async fn cancel(&self, job_id: &str, idempotency_key: &str) -> BridgeResult<()> {
        self.call(
            "jobs.control",
            json!({ "job_id": job_id, "action": "cancel", "idempotency_key": idempotency_key }),
            Duration::ZERO,
        )
        .await
        .map(|_| ())
    }

    /// Reads a Zeus approval this service requested, so Ogun's surfaces can say what a person must decide in Zeus.
    ///
    /// # Errors
    ///
    /// Zeus's `api.not_found` or transport failures.
    pub async fn approval(&self, approval_id: &str) -> BridgeResult<ZeusApprovalView> {
        let result = self
            .call(
                "approvals.get",
                json!({ "approval_id": approval_id }),
                Duration::ZERO,
            )
            .await?;
        let a = result
            .get("approval")
            .ok_or_else(|| BridgeError::Protocol("approvals.get result has no approval".into()))?;
        let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_owned);
        Ok(ZeusApprovalView {
            id: s("id").unwrap_or_default(),
            kind: s("kind").unwrap_or_default(),
            state: s("state").unwrap_or_default(),
            origin: s("origin"),
            expires_at: s("expires_at"),
        })
    }

    /// Prepares the import of a completed job: checks the file lies in Ogun's root, fetches Zeus's export form
    /// when the daemon offers `artifacts.*`, and builds the fallback record.
    ///
    /// # Errors
    ///
    /// [`BridgeError::NotCompleted`], [`BridgeError::PathOutsideRoot`], transport failures.
    pub async fn prepare_import(&self, job_id: &str) -> BridgeResult<PendingImport> {
        let view = self.job_view(job_id).await?;
        let result = match (&view.state, &view.result) {
            (JobState::Completed, Some(r)) => r.clone(),
            _ => return Err(BridgeError::NotCompleted),
        };
        let raw = PathBuf::from(&result.path);
        let meta = std::fs::symlink_metadata(&raw)?;
        if !meta.is_file() {
            return Err(BridgeError::PathOutsideRoot);
        }
        let canonical = std::fs::canonicalize(&raw)?;
        if !self.known_roots().iter().any(|r| canonical.starts_with(r)) {
            return Err(BridgeError::PathOutsideRoot);
        }
        let expected = Digests {
            sha256: zeus_types::Hex256::from_hex(&result.sha256)
                .map_err(|_| BridgeError::Protocol("job result sha256 is not hex".into()))?,
            blake3: zeus_types::Hex256::from_hex(&result.blake3)
                .map_err(|_| BridgeError::Protocol("job result blake3 is not hex".into()))?,
            size_bytes: result.size_bytes,
        };
        let export_form = if self.has_capability("artifacts").await {
            let page = self
                .call(
                    "artifacts.list",
                    json!({ "job_id": job_id, "limit": 10 }),
                    Duration::ZERO,
                )
                .await?;
            page.get("items")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items.iter().find(|a| {
                        a.get("blake3").and_then(Value::as_str) == Some(result.blake3.as_str())
                            && a.get("state").and_then(Value::as_str) == Some("active")
                    })
                })
                .cloned()
        } else {
            None
        };
        let daemon_version = self
            .session()
            .await
            .map_or_else(|| "unknown".to_owned(), |s| s.daemon_version);
        let provenance = view.provenance(result.completed_at);
        let draft = draft_from_view(&view, &result, &daemon_version)?;
        Ok(PendingImport {
            job_id: job_id.to_owned(),
            source_path: canonical,
            export_form,
            draft,
            expected,
            provenance,
        })
    }

    /// Imports a completed job's file into Ogun's artifact store.
    ///
    /// # Errors
    ///
    /// As [`ZeusBridge::prepare_import`] and [`PendingImport::commit`].
    pub async fn import(
        &self,
        job_id: &str,
        store: &ArtifactStore,
    ) -> BridgeResult<ImportedArtifact> {
        self.prepare_import(job_id).await?.commit(store)
    }
}

/// The Zeus-origin record for an ingest when no export form is available. It mirrors
/// `ArtifactDraft::from_completed_job` from what a service may see: the URLs are the log-redacted forms of the
/// job view, so `operation.params_blake3` hashes those (O-08 section 8.2).
fn draft_from_view(
    view: &JobView,
    result: &crate::job::ResultView,
    daemon_version: &str,
) -> BridgeResult<ArtifactDraft> {
    let name = Path::new(&result.path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_owned);
    let mut draft = ArtifactDraft::local("zeus", daemon_version, name.clone());
    let probe = view.probe.clone().unwrap_or_default();
    let validators = probe.validators.clone().unwrap_or_default();
    let session_origin = view
        .spec
        .session_origin
        .as_deref()
        .and_then(|o| Origin::parse(o).ok());
    let origin_only = session_origin
        .as_ref()
        .and_then(|o| RedactedUrl::new(&format!("{o}/")).ok());
    let url = view
        .spec
        .url_redacted
        .as_deref()
        .and_then(|u| RedactedUrl::new(u).ok())
        .or_else(|| origin_only.clone())
        .ok_or_else(|| BridgeError::Protocol("job view has no usable URL".into()))?;
    let final_url = probe
        .final_url_redacted
        .as_deref()
        .and_then(|u| RedactedUrl::new(u).ok())
        .unwrap_or_else(|| url.clone());
    let referrer = view
        .spec
        .referrer_redacted
        .as_deref()
        .and_then(|u| RedactedUrl::new(u).ok());
    let reduced = url.as_str().contains('\u{2026}') || final_url.as_str().contains('\u{2026}');
    let r = &mut draft.record;
    r.origin = ArtifactOrigin::Remote {
        url: url.clone(),
        final_url,
        url_redacted: reduced,
        referrer,
        session_origin: session_origin.clone(),
        http: Some(Box::new(ManifestHttp {
            status: probe.status,
            etag: validators.etag,
            last_modified: validators.last_modified,
            date: None,
            repr_digest: validators.repr_digest,
        })),
    };
    r.retrieved_at = Some(result.completed_at);
    r.job_id = JobId::from_str_opt(&view.id);
    r.producer.source_kind = view.spec.source.as_ref().and_then(|s| s.kind.clone());
    r.media_type = result.detected_mime.clone();
    r.declared_media_type = probe
        .content_type
        .as_deref()
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned());
    r.quarantined = view.quarantined;
    r.quarantine_reasons = view
        .quarantine_reasons
        .iter()
        .filter_map(|q| serde_json::from_value::<QuarantineReason>(Value::String(q.clone())).ok())
        .collect();
    r.metadata = view
        .spec
        .labels
        .iter()
        .take(32)
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let mut params = Map::new();
    params.insert("url".into(), Value::String(url.as_str().to_owned()));
    if let Some(h) = &view.spec.expected_sha256 {
        params.insert("expected_sha256".into(), Value::String(h.clone()));
        r.verification.basis = VerificationBasis::Expected;
    }
    if let Some(size) = view.spec.expected_size {
        params.insert("expected_size".into(), Value::from(size));
    }
    let params_hash = zeus_verify::canonical::request_hash(&Value::Object(params))
        .map_err(|_| BridgeError::Protocol("provenance parameters are not hashable".into()))?;
    r.operation = Operation {
        name: "zeus.download".into(),
        version: daemon_version.to_owned(),
        params_blake3: params_hash,
        tool: None,
    };
    if r.validate().is_err() {
        // A redacted form Zeus 13 still finds reducible, or labels it does not accept: keep the origin only.
        r.origin = ArtifactOrigin::Remote {
            url: origin_only.clone().unwrap_or_else(|| url.clone()),
            final_url: origin_only.unwrap_or(url),
            url_redacted: true,
            referrer: None,
            session_origin,
            http: None,
        };
        r.metadata = Map::new();
    }
    Ok(draft)
}

trait FromStrOpt: Sized {
    fn from_str_opt(text: &str) -> Option<Self>;
}

impl FromStrOpt for JobId {
    fn from_str_opt(text: &str) -> Option<Self> {
        serde_json::from_value(Value::String(text.to_owned())).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_outside_the_allow_list_are_refused() {
        let root = Path::new("/home/ada/Downloads/Zeus/ogun");
        let ok = json!({"url": "https://portal.example.org/files/r.pdf?sig=ZEUS-CANARY-1", "session_origin": "https://portal.example.org"});
        let job = new_job(&ok, root, &BTreeMap::new()).unwrap();
        assert_eq!(job["dest_dir"], "/home/ada/Downloads/Zeus/ogun");
        for bad in [
            json!({"url": "https://a.example/x", "session_origin": "https://a.example", "dest_dir": "/etc"}),
            json!({"url": "https://a.example/x", "session_origin": "https://a.example", "cookie": "x"}),
            json!({"url": "https://a.example/x", "session_origin": "https://a.example", "extra_headers": {"Authorization": "Bearer x"}}),
            json!({"url": "https://a.example/x", "session_origin": "https://a.example", "overwrite": "replace"}),
            json!({"url": "https://a.example/x"}),
            json!({"url": "https://a.example/x", "session_origin": "http://a.example"}),
        ] {
            let e = new_job(&bad, root, &BTreeMap::new()).unwrap_err();
            assert!(!e.to_string().contains("ZEUS-CANARY-"));
            assert_eq!(e.code(), "zeus.request_invalid");
        }
        let e = new_job(&ok, root, &BTreeMap::new());
        assert!(e.is_ok());
    }

    #[test]
    fn subdirs_are_plain_relative_names() {
        assert!(safe_subdir("orun_01J/ni_01J").is_ok());
        assert!(safe_subdir("../x").is_err());
        assert!(safe_subdir("/abs").is_err());
        assert!(safe_subdir(".hidden").is_err());
    }
}
