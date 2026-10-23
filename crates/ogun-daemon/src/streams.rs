//! Stream sources inside `ogund` (O-09, O-10).
//!
//! The ingest bridge runs the `stream.*` operators and pushes records with `streams.push`. Each
//! source has its own durable ingest log (`streams/<workflow>/<node>.redb`); a record is
//! acknowledged (`accepted_through`) only after its batch commits there. The log feeds a
//! [`Windower`]; when a window seals, the sealed JSONL is ingested into the artifact store and a run
//! is created for the window's partition with the source node already succeeded, so the batch nodes
//! downstream run like any partitioned workflow. On restart the windower is rebuilt by replaying
//! the unreleased log at the records' original ingest times; re-sealing the same records gives the
//! same digest, which is recognised and does not start a second run.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use ogun_api::methods::{PushParams, PushResult, StreamRecord, StreamView};
use ogun_api::{ApiError, ApiResult};
use ogun_edge::{Incoming, IngestLog, LogError};
use ogun_ir::Workflow;
use ogun_ir::model::{Partitions, WhenFull, WindowPartitions};
use ogun_run::{Actor, Cause};
use ogun_store::{Collection, DocWrite};
use ogun_stream::{Action, Ingested, Record, WindowSpec, Windower};
use ogun_types::{CredentialId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::store_err;
use crate::state::{Daemon, OutputRecord, lock};

const DEFAULT_BUFFER_BYTES: u64 = 256 << 20;
const MAX_PUSH: usize = 10_000;

/// A sealed window's last committed revision (`Collection::Streams`, `<wf>/<node>/win/<start>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WindowDoc {
    revision: u32,
    blake3: String,
}

struct Source {
    workflow_hash: String,
    op: String,
    spec: WindowPartitions,
    dedupe_key: Vec<String>,
    log: IngestLog,
    windower: Windower,
    /// Smallest log sequence number per non-final window, for releasing the log.
    window_min_seq: BTreeMap<i64, u64>,
    last_seq: Option<u64>,
    producers: HashMap<CredentialId, u64>,
    dropped: u64,
    sealed: u64,
    late_file: PathBuf,
}

/// Every active stream source, by `(workflow, node)`.
pub struct Streams {
    dir: PathBuf,
    inner: Mutex<HashMap<(String, String), Source>>,
}

impl std::fmt::Debug for Streams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Streams")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn now_ms() -> i64 {
    Timestamp::now().unix_millis()
}

fn spec_of(w: &WindowPartitions) -> WindowSpec {
    WindowSpec {
        size_ms: w.size_ms,
        allowed_lateness_ms: w.allowed_lateness_ms,
        reseal_after_ms: w.reseal_after_ms,
        max_delay_ms: w.max_delay_ms,
        idle_timeout_ms: w.idle_timeout_ms,
        max_future_ms: w.max_future_ms,
    }
}

fn field<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(payload, |v, seg| v.get(seg))
}

fn to_record(r: &StreamRecord, seq: u64, dedupe_key: &[String]) -> Record {
    let dedupe = if dedupe_key.is_empty() {
        None
    } else {
        let parts: Option<Vec<String>> = dedupe_key
            .iter()
            .map(|f| {
                field(&r.payload, f).map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            })
            .collect();
        parts.map(|p| p.join("/"))
    };
    Record {
        key: r.key.clone(),
        event_ms: r.event_time_ms,
        seq,
        dedupe,
        payload: r.payload.clone(),
        clamped: false,
    }
}

fn log_err(e: &LogError) -> ApiError {
    match e {
        LogError::Full { .. } => ApiError::new("stream.full", "the stream buffer is full"),
        other => {
            tracing::error!(error = %other, "ingest log failure");
            ApiError::internal("ingest log failure")
        }
    }
}

/// The window partitions of a stream workflow whose source is `node`.
fn window_of<'a>(wf: &'a Workflow, node: &str) -> Option<&'a WindowPartitions> {
    match &wf.partitions {
        Some(Partitions::Window(w)) if w.source == node => Some(w),
        _ => None,
    }
}

impl Streams {
    /// No sources yet; they activate on first push or at startup.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn activate(&self, d: &Daemon, workflow: &str, node: &str) -> ApiResult<Source> {
        let version = d.version_row(workflow, None)?;
        if !d.runnable(&version) {
            return Err(ApiError::new(
                "workflow.not_approved",
                "the stream workflow's latest version is not approved",
            ));
        }
        let wf = d.workflow(&version.workflow_hash)?;
        let spec = window_of(&wf, node)
            .ok_or_else(|| ApiError::new("stream.unknown", "no stream source by that name"))?
            .clone();
        let op = wf
            .nodes
            .iter()
            .find(|n| n.id == node)
            .map(|n| n.op.clone())
            .unwrap_or_default();
        let buffer = wf.streams.get(node);
        let dir = self.dir.join(workflow);
        crate::creds::private_dir(&dir)?;
        let log = IngestLog::open(
            &dir.join(format!("{node}.redb")),
            buffer.map_or(DEFAULT_BUFFER_BYTES, |b| b.max_buffer_bytes),
            buffer.map_or(WhenFull::Block, |b| b.when_full),
        )
        .map_err(|e| log_err(&e))?;
        let mut windower = Windower::new(spec_of(&spec))
            .map_err(|_| ApiError::internal("stored window spec is invalid"))?;
        let dedupe_key = buffer.map(|b| b.dedupe_key.clone()).unwrap_or_default();
        // Replay the unreleased log at the records' original ingest times.
        let mut window_min_seq = BTreeMap::new();
        let mut last_seq = None;
        let from = log
            .stats()
            .map_err(|e| log_err(&e))?
            .released_through
            .map_or(0, |r| r + 1);
        let mut next = from;
        loop {
            let batch = log.read(next, 10_000).map_err(|e| log_err(&e))?;
            if batch.is_empty() {
                break;
            }
            for logged in &batch {
                next = logged.seq + 1;
                last_seq = Some(logged.seq);
                let Ok(rec) = serde_json::from_value::<StreamRecord>(logged.record.payload.clone())
                else {
                    continue;
                };
                if let Ingested::Accepted { window_start } | Ingested::Revises { window_start } =
                    windower.ingest(
                        to_record(&rec, logged.seq, &dedupe_key),
                        logged.record.received_ms,
                    )
                {
                    let e = window_min_seq.entry(window_start).or_insert(logged.seq);
                    *e = (*e).min(logged.seq);
                }
            }
        }
        // Replayed late records were written to the late file the first time.
        let _ = windower.take_late();
        let mut src = Source {
            workflow_hash: version.workflow_hash.clone(),
            op,
            spec,
            dedupe_key,
            log,
            windower,
            window_min_seq,
            last_seq,
            producers: HashMap::new(),
            dropped: 0,
            sealed: 0,
            late_file: dir.join(format!("{node}.late.jsonl")),
        };
        for (key, pos) in d
            .store
            .list_docs::<u64>(Collection::Streams, &format!("{workflow}/{node}/producer/"))
            .map_err(store_err)?
        {
            if let Some(id) = key.rsplit('/').next().and_then(|s| s.parse().ok()) {
                src.producers.insert(id, pos);
            }
        }
        tracing::info!(
            workflow,
            node,
            replayed = next - from,
            "stream source active"
        );
        Ok(src)
    }

    fn with_source<T>(
        &self,
        d: &Daemon,
        workflow: &str,
        node: &str,
        f: impl FnOnce(&mut Source) -> ApiResult<T>,
    ) -> ApiResult<T> {
        let mut map = lock(&self.inner);
        let key = (workflow.to_owned(), node.to_owned());
        if !map.contains_key(&key) {
            let src = self.activate(d, workflow, node)?;
            map.insert(key.clone(), src);
        }
        let src = map
            .get_mut(&key)
            .ok_or_else(|| ApiError::internal("stream vanished"))?;
        f(src)
    }

    /// `streams.push`.
    ///
    /// # Errors
    ///
    /// `stream.unknown`, `workflow.not_approved`, `api.invalid_params` for empty or oversized batches.
    pub fn push(
        &self,
        d: &Daemon,
        producer: CredentialId,
        p: &PushParams,
    ) -> ApiResult<PushResult> {
        if p.records.len() > MAX_PUSH {
            return Err(ApiError::invalid_params("at most 10 000 records per push"));
        }
        if p.records.is_empty() {
            return Err(ApiError::invalid_params(
                "a push carries at least one record",
            ));
        }
        let now = now_ms();
        self.with_source(d, &p.workflow, &p.node, |src| {
            // `next` is one past the producer's last accepted sequence number; 0 before any push.
            let next = src.producers.get(&producer).copied().unwrap_or(0);
            let skip = usize::try_from(next.saturating_sub(p.first_seq)).unwrap_or(usize::MAX);
            let fresh: Vec<&StreamRecord> = p.records.iter().skip(skip).collect();
            let through = p.first_seq + p.records.len() as u64 - 1;
            if fresh.is_empty() {
                return Ok(PushResult {
                    accepted_through: through,
                    dropped: 0,
                    late: 0,
                    retry_after_ms: 0,
                });
            }
            let batch: Vec<Incoming> = fresh
                .iter()
                .map(|r| Incoming {
                    received_ms: now,
                    origin: producer.to_string(),
                    payload: serde_json::to_value(r).unwrap_or(Value::Null),
                })
                .collect();
            let appended = match src.log.append(batch) {
                Ok(a) => a,
                Err(LogError::Full { .. }) => {
                    return Ok(PushResult {
                        accepted_through: next.saturating_sub(1),
                        dropped: 0,
                        late: 0,
                        retry_after_ms: 1_000,
                    });
                }
                Err(e) => return Err(log_err(&e)),
            };
            let dropped = appended.dropped_newest;
            src.dropped += dropped + appended.evicted_oldest;
            let mut late = 0u64;
            let mut late_lines = Vec::new();
            if let Some(first) = appended.first_seq {
                for (i, r) in fresh
                    .iter()
                    .take(usize::try_from(appended.written).unwrap_or(usize::MAX))
                    .enumerate()
                {
                    let seq = first + i as u64;
                    match src.windower.ingest(to_record(r, seq, &src.dedupe_key), now) {
                        Ingested::Accepted { window_start }
                        | Ingested::Revises { window_start } => {
                            let e = src.window_min_seq.entry(window_start).or_insert(seq);
                            *e = (*e).min(seq);
                        }
                        Ingested::Late => late += 1,
                        Ingested::Duplicate => {}
                    }
                    src.last_seq = Some(seq);
                }
                for r in src.windower.take_late() {
                    late_lines.push(r);
                }
            }
            if !late_lines.is_empty() {
                append_late(&src.late_file, &late_lines);
            }
            src.producers.insert(producer, through + 1);
            d.store
                .put_doc(
                    Collection::Streams,
                    &format!("{}/{}/producer/{producer}", p.workflow, p.node),
                    &(through + 1),
                )
                .map_err(store_err)?;
            Ok(PushResult {
                accepted_through: through,
                dropped,
                late,
                retry_after_ms: 0,
            })
        })
    }

    /// Activates every stream workflow whose latest version is enabled and approved.
    pub fn activate_all(&self, d: &Daemon) {
        let Ok(rows) = d
            .store
            .list_docs::<crate::state::WorkflowRow>(Collection::Workflows, "")
        else {
            return;
        };
        for (_, row) in rows.into_iter().filter(|(_, r)| r.enabled) {
            let Ok(wf) = d.workflow(&row.latest_hash) else {
                continue;
            };
            if let Some(Partitions::Window(w)) = &wf.partitions {
                let node = w.source.clone();
                if let Err(e) = self.with_source(d, &row.name, &node, |_| Ok(())) {
                    tracing::warn!(workflow = %row.name, error = %e, "stream source not activated");
                }
            }
        }
    }

    /// Seals what is due and releases what is final. Called about once a second.
    pub fn poll(&self, d: &Daemon) {
        let now = now_ms();
        let mut map = lock(&self.inner);
        let mut changed = false;
        for ((workflow, node), src) in map.iter_mut() {
            for action in src.windower.poll(now) {
                match action {
                    Action::Seal {
                        window_start,
                        records,
                        ..
                    } => match seal_window(d, workflow, node, src, window_start, &records) {
                        Ok(new_run) => changed |= new_run,
                        Err(e) => {
                            tracing::error!(workflow = %workflow, node = %node, error = %e, "window seal failed")
                        }
                    },
                    Action::Finalized { window_start } => {
                        src.window_min_seq.remove(&window_start);
                    }
                }
            }
            src.windower.compact();
            let release = match src.window_min_seq.values().min() {
                Some(min) => min.checked_sub(1),
                None => src.last_seq,
            };
            if let Some(through) = release
                && let Err(e) = src.log.release_through(through)
            {
                tracing::warn!(error = %e, "ingest log release failed");
            }
        }
        drop(map);
        if changed {
            d.changed();
        }
    }

    /// `streams.list`.
    #[must_use]
    pub fn list(&self) -> Vec<StreamView> {
        let now = now_ms();
        let map = lock(&self.inner);
        let mut out: Vec<StreamView> = map
            .iter()
            .map(|((workflow, node), src)| view(workflow, node, src, now))
            .collect();
        out.sort_by(|a, b| (&a.workflow, &a.node).cmp(&(&b.workflow, &b.node)));
        out
    }

    /// `streams.status`.
    ///
    /// # Errors
    ///
    /// `stream.unknown`.
    pub fn status(&self, d: &Daemon, workflow: &str, node: &str) -> ApiResult<StreamView> {
        let now = now_ms();
        self.with_source(d, workflow, node, |src| Ok(view(workflow, node, src, now)))
    }
}

fn view(workflow: &str, node: &str, src: &Source, now: i64) -> StreamView {
    let counters = src.windower.counters();
    StreamView {
        workflow: workflow.to_owned(),
        node: node.to_owned(),
        op: src.op.clone(),
        buffered: src.log.len().unwrap_or(0),
        dropped: src.dropped,
        watermark_ms: Some(src.windower.watermark(now)),
        windows_sealed: src.sealed.max(counters.revisions),
        windows_open: src.window_min_seq.len() as u64,
    }
}

fn append_late(path: &PathBuf, records: &[Record]) {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(path) {
        for r in records {
            if let Ok(line) = serde_json::to_vec(r) {
                let _ = f.write_all(&line);
                let _ = f.write_all(b"\n");
            }
        }
    }
}

/// Seals one window revision. Returns whether a new run was created.
fn seal_window(
    d: &Daemon,
    workflow: &str,
    node: &str,
    src: &mut Source,
    window_start: i64,
    records: &[Record],
) -> ApiResult<bool> {
    let batch = ogun_stream::seal(records)
        .map_err(|_| ApiError::internal("a record is not canonicalisable"))?;
    let doc_key = format!("{workflow}/{node}/win/{window_start}");
    let previous: Option<WindowDoc> = d
        .store
        .get_doc(Collection::Streams, &doc_key)
        .map_err(store_err)?;
    if previous.as_ref().is_some_and(|p| p.blake3 == batch.blake3) {
        src.windower.seal_committed(window_start, &batch.blake3);
        return Ok(false);
    }
    let revision = previous.map_or(1, |p| p.revision + 1);
    let partition = ogun_stream::window_key(window_start);
    // The run binds to the latest approved version; if that changed, the next activation picks it up.
    let wf = d.workflow(&src.workflow_hash)?;
    let now = Timestamp::now();
    let run = d
        .store
        .create_run(&wf, std::slice::from_ref(&partition), &d.ids, now)
        .map_err(store_err)?;
    let tmp_dir = d.paths.scratch_dir().join("seal");
    crate::creds::private_dir(&tmp_dir)?;
    let tmp = tmp_dir.join(format!("{}.jsonl", batch.blake3));
    std::fs::write(&tmp, &batch.bytes).map_err(|e| {
        ApiError::unavailable(format!("cannot write a sealed window: {}", e.kind()))
    })?;
    let mut draft = zeus_artifact::ArtifactDraft::local(
        "ogun",
        env!("CARGO_PKG_VERSION"),
        Some(format!("{partition}.jsonl")),
    );
    draft.record.declared_media_type = Some("application/x-ndjson".into());
    for (k, v) in [
        ("ogun.workflow", workflow.to_owned()),
        ("ogun.node", node.to_owned()),
        ("ogun.partition", partition.clone()),
        ("ogun.output", "window".to_owned()),
        ("ogun.run_id", run.id.to_string()),
    ] {
        draft.record.metadata.insert(k.into(), Value::String(v));
    }
    draft
        .record
        .metadata
        .insert("ogun.revision".into(), Value::from(revision));
    let artifact = d
        .artifacts
        .ingest(
            &tmp,
            draft,
            Some(&format!("ogun-seal-{}-{}", batch.blake3, run.id)),
        )
        .map_err(|e| crate::error::artifact_err(&e))?;
    let _ = std::fs::remove_file(&tmp);
    let instances = d.store.instances(run.id).map_err(store_err)?;
    let Some(source) = instances.iter().find(|i| i.node == node) else {
        return Err(ApiError::internal("stream run has no source instance"));
    };
    let output = OutputRecord {
        port: "window".into(),
        blake3: batch.blake3.clone(),
        size_bytes: batch.bytes.len() as u64,
        media_type: "application/x-ndjson".into(),
        metadata: Some(serde_json::json!({
            "partition": partition,
            "revision": revision,
            "records": batch.records,
            "window_start_ms": window_start,
            "window_end_ms": window_start + i64::try_from(src.spec.size_ms).unwrap_or(i64::MAX),
        })),
        route: None,
        artifact_id: artifact.id.to_string(),
        committed_at: now,
    };
    d.store
        .transition(source.id, Cause::InputsAvailable, Actor::Runtime, now, None)
        .map_err(store_err)?;
    d.store
        .transition(source.id, Cause::Dispatched, Actor::Runtime, now, None)
        .map_err(store_err)?;
    let writes = [
        DocWrite::put(Collection::Outputs, source.id.to_string(), &vec![output])
            .map_err(store_err)?,
        DocWrite::put(
            Collection::Streams,
            doc_key,
            &WindowDoc {
                revision,
                blake3: batch.blake3.clone(),
            },
        )
        .map_err(store_err)?,
    ];
    d.store
        .transition_with(
            source.id,
            Cause::OutputsCommitted,
            Actor::Runtime,
            now,
            None,
            &writes,
        )
        .map_err(store_err)?;
    src.windower.seal_committed(window_start, &batch.blake3);
    src.sealed += 1;
    tracing::info!(workflow, node, partition = %partition, revision, records = batch.records, "window sealed");
    Ok(true)
}
