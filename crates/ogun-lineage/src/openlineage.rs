//! OpenLineage 2.x run events from the run store (O-05 section 4).
//!
//! Mapping, in short:
//!
//! * One Ogun run (`orun_`) is one OpenLineage run of the job `<workflow>` in the configured namespace. Its
//!   `START` is the store's `run_created` event; its `COMPLETE`, `FAIL` or `ABORT` is emitted once every node
//!   instance is terminal, with the state derived as O-01 section 5.3 says.
//! * One node instance (`ni_`) is one OpenLineage run of the job `<workflow>.<node>`, with a `parent` facet that
//!   points at the Ogun run. The first `dispatched` is `START`; later dispatches, session resumptions and moves out
//!   of `running` into a non-terminal state are `RUNNING`; `outputs_committed` is `COMPLETE`; `failed` is `FAIL`;
//!   `cancelled` after a start is `ABORT`. Instances that never started and were skipped or cancelled emit nothing.
//! * A dataset is one output port: `<workflow>.<node>.<output>`. Its version is the BLAKE3 of the JCS form of the
//!   sorted list of `{blake3, size_bytes}` it committed for the partition; the `ogun_artifacts` facet lists the
//!   artifacts themselves (at most [`LineageConfig::max_artifacts_per_dataset`]).
//! * OpenLineage `runId`s are UUIDs: [`uuid_from_ulid`] keeps the ULID's timestamp and most of its randomness and
//!   sets the UUIDv7 version and variant bits. The Ogun identifier itself is in the `ogun_run` or `ogun_node_run`
//!   facet, so nothing depends on reversing the mapping.
//!
//! OpenLineage's own members are camelCase (`eventType`, `runId`); Ogun's custom facets use `snake_case` member
//! names and are prefixed `ogun_`, as the OpenLineage facet naming rules ask of custom facets.

use std::collections::{BTreeMap, HashMap, HashSet};

use ogun_ir::Workflow;
use ogun_run::{Cause, NodeState, RunState, derive_run_state};
use ogun_store::{Event, EventKind, NodeInstance, RunRecord};
use ogun_types::{NodeInstanceId, OgunError, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ulid::Ulid;

use crate::records::AgentRunRecord;
use crate::{LineageError, LineageResult};

/// The OpenLineage `RunEvent` schema this crate writes.
pub const RUN_EVENT_SCHEMA_URL: &str =
    "https://openlineage.io/spec/2-0-2/OpenLineage.json#/$defs/RunEvent";
/// Default `producer` URI: the source of this crate.
pub const DEFAULT_PRODUCER: &str =
    "https://github.com/ArefinAlter/ogun/tree/main/crates/ogun-lineage";
/// Where the Ogun custom facets are described.
pub const OGUN_FACETS_SCHEMA_URL: &str = "https://raw.githubusercontent.com/ArefinAlter/ogun/main/docs/spec/schema/ogun-lineage-facets.v0.schema.json";
/// Default job namespace.
pub const DEFAULT_NAMESPACE: &str = "ogun://local";

const PARENT_SCHEMA: &str =
    "https://openlineage.io/spec/facets/1-1-0/ParentRunFacet.json#/$defs/ParentRunFacet";
const JOB_TYPE_SCHEMA: &str =
    "https://openlineage.io/spec/facets/2-0-3/JobTypeJobFacet.json#/$defs/JobTypeJobFacet";
const ERROR_SCHEMA: &str = "https://openlineage.io/spec/facets/1-0-1/ErrorMessageRunFacet.json#/$defs/ErrorMessageRunFacet";
const ENGINE_SCHEMA: &str = "https://openlineage.io/spec/facets/1-1-1/ProcessingEngineRunFacet.json#/$defs/ProcessingEngineRunFacet";
const VERSION_SCHEMA: &str = "https://openlineage.io/spec/facets/1-0-1/DatasetVersionDatasetFacet.json#/$defs/DatasetVersionDatasetFacet";

/// OpenLineage event types. Ogun emits all but `OTHER`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum EventType {
    /// The run started.
    Start,
    /// A status update of a started run.
    Running,
    /// The run finished successfully.
    Complete,
    /// The run was stopped before it finished.
    Abort,
    /// The run failed.
    Fail,
    /// Anything else.
    Other,
}

/// An OpenLineage run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// A UUID.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// Run facets by name.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub facets: Map<String, Value>,
}

/// An OpenLineage job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// Namespace, for example `ogun://local`.
    pub namespace: String,
    /// Name, `<workflow>` or `<workflow>.<node>`.
    pub name: String,
    /// Job facets by name.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub facets: Map<String, Value>,
}

/// An input or output dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dataset {
    /// Namespace.
    pub namespace: String,
    /// Name, `<workflow>.<node>.<output>`.
    pub name: String,
    /// Dataset facets.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub facets: Map<String, Value>,
}

/// One OpenLineage `RunEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    /// What happened.
    pub event_type: EventType,
    /// RFC 3339, UTC, milliseconds.
    pub event_time: String,
    /// The run.
    pub run: Run,
    /// The job.
    pub job: Job,
    /// Datasets read.
    #[serde(default)]
    pub inputs: Vec<Dataset>,
    /// Datasets written.
    #[serde(default)]
    pub outputs: Vec<Dataset>,
    /// URI of the producing software.
    pub producer: String,
    /// Schema of this document.
    #[serde(rename = "schemaURL")]
    pub schema_url: String,
}

/// Where events are attributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageConfig {
    /// Job and dataset namespace. Default `ogun://local`; a server workspace uses `ogun://<workspace>`.
    pub namespace: String,
    /// `producer` URI.
    pub producer: String,
    /// Most artifacts listed in one dataset's `ogun_artifacts` facet; the rest are counted.
    pub max_artifacts_per_dataset: usize,
}

impl Default for LineageConfig {
    fn default() -> Self {
        Self {
            namespace: DEFAULT_NAMESPACE.to_owned(),
            producer: DEFAULT_PRODUCER.to_owned(),
            max_artifacts_per_dataset: 256,
        }
    }
}

/// The exact operator a node ran with (O-03 section 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorInfo {
    /// The IR reference, `name@major`.
    pub op: String,
    /// The exact version that ran, empty when unknown.
    pub version: String,
    /// BLAKE3 of the manifest's JCS form, for non-core operators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_blake3: Option<String>,
    /// BLAKE3 of the code bundle, for `code` operators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_blake3: Option<String>,
}

/// The time window a partition covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionWindow {
    /// Inclusive start, Unix ms.
    pub start_ms: i64,
    /// Exclusive end, Unix ms.
    pub end_ms: i64,
}

/// One artifact a node instance committed. A plain row, so this crate needs neither the daemon nor the artifact
/// store: the caller copies it from its artifact index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputArtifact {
    /// The producing node instance (its node and partition come from the run store).
    pub node_instance: NodeInstanceId,
    /// Output port.
    pub output: String,
    /// Store-local id (`art_`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    /// BLAKE3, 64 lowercase hex digits: the identity across stores.
    pub blake3: String,
    /// SHA-256, when the store computed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Size.
    pub size_bytes: u64,
    /// Media type.
    pub media_type: String,
    /// When the artifact store committed it.
    pub committed_at: Timestamp,
}

/// Everything known about one run, ready to become OpenLineage events.
#[derive(Debug, Clone)]
pub struct RunLineage {
    /// The run.
    pub run: RunRecord,
    /// Its node instances, current state.
    pub instances: Vec<NodeInstance>,
    /// Its events (events of other runs are ignored).
    pub events: Vec<Event>,
    /// Committed outputs.
    pub outputs: Vec<OutputArtifact>,
    /// Operators by node id.
    pub operators: BTreeMap<String, OperatorInfo>,
    /// Input references (`node.output`) by node id.
    pub node_inputs: BTreeMap<String, Vec<String>>,
    /// Windows by partition key.
    pub windows: BTreeMap<String, PartitionWindow>,
    /// The final error of failed node instances.
    pub errors: BTreeMap<NodeInstanceId, OgunError>,
}

/// A UUID string for an OpenLineage `runId`: the ULID's 128 bits with the UUIDv7 version (`7`) and variant (`10`)
/// bits set. ULIDs and UUIDv7 both start with a 48-bit millisecond timestamp, so ordering across
/// different milliseconds is kept. Overwriting random bits does not preserve order within a millisecond.
#[must_use]
pub fn uuid_from_ulid(ulid: Ulid) -> String {
    let mut b = u128::from(ulid).to_be_bytes();
    b[6] = (b[6] & 0x0f) | 0x70;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn facet(producer: &str, schema: &str, fields: Value) -> Value {
    let mut map = Map::new();
    map.insert("_producer".into(), Value::String(producer.to_owned()));
    map.insert("_schemaURL".into(), Value::String(schema.to_owned()));
    if let Value::Object(fields) = fields {
        map.extend(fields);
    }
    Value::Object(map)
}

fn ogun_facet(producer: &str, def: &str, fields: Value) -> Value {
    facet(
        producer,
        &format!("{OGUN_FACETS_SCHEMA_URL}#/$defs/{def}"),
        fields,
    )
}

fn state_name(state: NodeState) -> &'static str {
    state.as_str()
}

fn cause_name(cause: Cause) -> String {
    serde_json::to_value(cause)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The dataset version: BLAKE3 over the JCS form of the sorted `{blake3, size_bytes}` list.
fn manifest_digest(arts: &[&OutputArtifact]) -> String {
    let mut items: Vec<(String, u64)> = arts
        .iter()
        .map(|a| (a.blake3.clone(), a.size_bytes))
        .collect();
    items.sort();
    let list: Vec<Value> = items
        .into_iter()
        .map(|(b, s)| json!({ "blake3": b, "size_bytes": s }))
        .collect();
    match zeus_verify::canonical::to_jcs_bytes(&Value::Array(list)) {
        Ok(bytes) => blake3::hash(&bytes).to_hex().to_string(),
        Err(_) => String::new(),
    }
}

impl RunLineage {
    /// Starts from the run store's rows. Events of other runs are dropped.
    #[must_use]
    pub fn new(run: RunRecord, instances: Vec<NodeInstance>, events: Vec<Event>) -> Self {
        let id = run.id;
        Self {
            run,
            instances,
            events: events.into_iter().filter(|e| e.run == id).collect(),
            outputs: vec![],
            operators: BTreeMap::new(),
            node_inputs: BTreeMap::new(),
            windows: BTreeMap::new(),
            errors: BTreeMap::new(),
        }
    }

    /// Takes node inputs and operator references from the workflow version the run is bound to. Exact operator
    /// versions and manifest hashes set with [`RunLineage::with_operator`] win over what the IR alone can say.
    #[must_use]
    pub fn with_workflow(mut self, wf: &Workflow) -> Self {
        for node in &wf.nodes {
            self.node_inputs
                .insert(node.id.clone(), node.inputs.clone());
            self.operators
                .entry(node.id.clone())
                .or_insert_with(|| OperatorInfo {
                    op: node.op.clone(),
                    version: String::new(),
                    manifest_blake3: None,
                    code_blake3: node.code.as_ref().map(|c| c.blake3.clone()),
                });
        }
        self
    }

    /// Records the exact operator a node ran with.
    #[must_use]
    pub fn with_operator(mut self, node: &str, info: OperatorInfo) -> Self {
        self.operators.insert(node.to_owned(), info);
        self
    }

    /// Adds committed outputs.
    #[must_use]
    pub fn with_outputs(mut self, outputs: impl IntoIterator<Item = OutputArtifact>) -> Self {
        self.outputs.extend(outputs);
        self
    }

    /// Records the window of a partition.
    #[must_use]
    pub fn with_window(mut self, partition: &str, window: PartitionWindow) -> Self {
        self.windows.insert(partition.to_owned(), window);
        self
    }

    /// Records the final error of a failed node instance (its message is already free of secrets, O-00).
    #[must_use]
    pub fn with_error(mut self, instance: NodeInstanceId, error: OgunError) -> Self {
        self.errors.insert(instance, error);
        self
    }

    fn engine_facet(cfg: &LineageConfig) -> Value {
        facet(
            &cfg.producer,
            ENGINE_SCHEMA,
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "name": "ogun",
                "openlineageAdapterVersion": env!("CARGO_PKG_VERSION"),
            }),
        )
    }

    fn job_type(cfg: &LineageConfig, kind: &str) -> Value {
        facet(
            &cfg.producer,
            JOB_TYPE_SCHEMA,
            json!({ "processingType": "BATCH", "integration": "OGUN", "jobType": kind }),
        )
    }

    fn workflow_event(
        &self,
        cfg: &LineageConfig,
        ty: EventType,
        at: Timestamp,
        state: RunState,
    ) -> RunEvent {
        let mut run_facets = Map::new();
        run_facets.insert(
            "ogun_run".into(),
            ogun_facet(
                &cfg.producer,
                "OgunRunFacet",
                json!({
                    "run_id": self.run.id.to_string(),
                    "workflow": self.run.workflow,
                    "workflow_version": self.run.version,
                    "workflow_hash": self.run.workflow_hash,
                    "partitions": self.run.partitions.len(),
                    "state": serde_json::to_value(state).unwrap_or(Value::Null),
                }),
            ),
        );
        run_facets.insert("processing_engine".into(), Self::engine_facet(cfg));
        let mut job_facets = Map::new();
        job_facets.insert("jobType".into(), Self::job_type(cfg, "WORKFLOW"));
        RunEvent {
            event_type: ty,
            event_time: at.to_rfc3339(),
            run: Run {
                run_id: uuid_from_ulid(self.run.id.ulid()),
                facets: run_facets,
            },
            job: Job {
                namespace: cfg.namespace.clone(),
                name: self.run.workflow.clone(),
                facets: job_facets,
            },
            inputs: vec![],
            outputs: vec![],
            producer: cfg.producer.clone(),
            schema_url: RUN_EVENT_SCHEMA_URL.to_owned(),
        }
    }

    fn dataset(
        &self,
        cfg: &LineageConfig,
        node: &str,
        output: &str,
        partition: &str,
        arts: &[&OutputArtifact],
    ) -> Dataset {
        let mut facets = Map::new();
        if !arts.is_empty() {
            let mut sorted: Vec<&&OutputArtifact> = arts.iter().collect();
            sorted.sort_by(|a, b| a.blake3.cmp(&b.blake3));
            let listed: Vec<Value> = sorted
                .iter()
                .take(cfg.max_artifacts_per_dataset)
                .map(|a| {
                    let mut m = Map::new();
                    m.insert("blake3".into(), Value::String(a.blake3.clone()));
                    if let Some(s) = &a.sha256 {
                        m.insert("sha256".into(), Value::String(s.clone()));
                    }
                    m.insert("size_bytes".into(), Value::from(a.size_bytes));
                    m.insert("media_type".into(), Value::String(a.media_type.clone()));
                    if let Some(id) = &a.artifact_id {
                        m.insert("artifact_id".into(), Value::String(id.clone()));
                    }
                    m.insert(
                        "committed_at".into(),
                        Value::String(a.committed_at.to_rfc3339()),
                    );
                    Value::Object(m)
                })
                .collect();
            let total: u64 = arts
                .iter()
                .map(|a| a.size_bytes)
                .fold(0, u64::saturating_add);
            facets.insert(
                "ogun_artifacts".into(),
                ogun_facet(
                    &cfg.producer,
                    "OgunArtifactsDatasetFacet",
                    json!({
                        "partition": partition,
                        "count": arts.len(),
                        "total_bytes": total,
                        "truncated": arts.len() > cfg.max_artifacts_per_dataset,
                        "artifacts": listed,
                    }),
                ),
            );
            facets.insert(
                "version".into(),
                facet(
                    &cfg.producer,
                    VERSION_SCHEMA,
                    json!({ "datasetVersion": manifest_digest(arts) }),
                ),
            );
        }
        Dataset {
            namespace: cfg.namespace.clone(),
            name: format!("{}.{node}.{output}", self.run.workflow),
            facets,
        }
    }

    fn outputs_of<'a>(
        &'a self,
        by_id: &HashMap<NodeInstanceId, &'a NodeInstance>,
        node: &str,
        partition: &str,
    ) -> BTreeMap<String, Vec<&'a OutputArtifact>> {
        let mut ports: BTreeMap<String, Vec<&OutputArtifact>> = BTreeMap::new();
        for a in &self.outputs {
            if let Some(ni) = by_id.get(&a.node_instance)
                && ni.node == node
                && ni.partition == partition
            {
                ports.entry(a.output.clone()).or_default().push(a);
            }
        }
        ports
    }

    fn input_datasets(
        &self,
        cfg: &LineageConfig,
        by_id: &HashMap<NodeInstanceId, &NodeInstance>,
        ni: &NodeInstance,
    ) -> Vec<Dataset> {
        let Some(refs) = self.node_inputs.get(&ni.node) else {
            return vec![];
        };
        refs.iter()
            .filter_map(|r| r.split_once('.'))
            .map(|(up, port)| {
                let mut by_port = self.outputs_of(by_id, up, &ni.partition);
                if by_port.is_empty() && !ni.partition.is_empty() {
                    // An unpartitioned upstream feeds every partition.
                    by_port = self.outputs_of(by_id, up, "");
                }
                let arts = by_port.remove(port).unwrap_or_default();
                let partition = arts
                    .first()
                    .and_then(|a| by_id.get(&a.node_instance))
                    .map_or(ni.partition.as_str(), |p| p.partition.as_str());
                self.dataset(cfg, up, port, partition, &arts)
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn node_event(
        &self,
        cfg: &LineageConfig,
        by_id: &HashMap<NodeInstanceId, &NodeInstance>,
        ni: &NodeInstance,
        ty: EventType,
        at: Timestamp,
        cause: Cause,
        to: NodeState,
        attempt: u32,
    ) -> RunEvent {
        let mut run_facets = Map::new();
        run_facets.insert(
            "parent".into(),
            facet(
                &cfg.producer,
                PARENT_SCHEMA,
                json!({
                    "run": { "runId": uuid_from_ulid(self.run.id.ulid()) },
                    "job": { "namespace": cfg.namespace, "name": self.run.workflow },
                }),
            ),
        );
        run_facets.insert(
            "ogun_node_run".into(),
            ogun_facet(
                &cfg.producer,
                "OgunNodeRunFacet",
                json!({
                    "node_instance": ni.id.to_string(),
                    "run_id": self.run.id.to_string(),
                    "workflow_hash": self.run.workflow_hash,
                    "node": ni.node,
                    "partition": ni.partition,
                    "attempt": attempt,
                    "state": state_name(to),
                    "cause": cause_name(cause),
                }),
            ),
        );
        if let Some(op) = self.operators.get(&ni.node) {
            run_facets.insert(
                "ogun_operator".into(),
                ogun_facet(
                    &cfg.producer,
                    "OgunOperatorRunFacet",
                    serde_json::to_value(op).unwrap_or(Value::Null),
                ),
            );
        }
        if !ni.partition.is_empty() {
            let mut fields = Map::new();
            fields.insert("key".into(), Value::String(ni.partition.clone()));
            if let Some(w) = self.windows.get(&ni.partition) {
                fields.insert("window_start_ms".into(), Value::from(w.start_ms));
                fields.insert("window_end_ms".into(), Value::from(w.end_ms));
            }
            run_facets.insert(
                "ogun_partition".into(),
                ogun_facet(
                    &cfg.producer,
                    "OgunPartitionRunFacet",
                    Value::Object(fields),
                ),
            );
        }
        if ty == EventType::Fail
            && let Some(err) = self.errors.get(&ni.id)
        {
            run_facets.insert(
                "errorMessage".into(),
                facet(
                    &cfg.producer,
                    ERROR_SCHEMA,
                    json!({ "message": format!("{}: {}", err.code, err.message), "programmingLanguage": "unknown" }),
                ),
            );
            run_facets.insert(
                "ogun_error".into(),
                ogun_facet(
                    &cfg.producer,
                    "OgunErrorRunFacet",
                    json!({ "code": err.code, "class": err.class, "retryable": err.retryable }),
                ),
            );
        }
        run_facets.insert("processing_engine".into(), Self::engine_facet(cfg));

        let mut job_facets = Map::new();
        job_facets.insert("jobType".into(), Self::job_type(cfg, "NODE"));

        let inputs = if matches!(ty, EventType::Start | EventType::Complete) {
            self.input_datasets(cfg, by_id, ni)
        } else {
            vec![]
        };
        let outputs = if ty == EventType::Complete {
            self.outputs_of(by_id, &ni.node, &ni.partition)
                .iter()
                .map(|(port, arts)| {
                    let own: Vec<&OutputArtifact> = arts
                        .iter()
                        .copied()
                        .filter(|a| a.node_instance == ni.id)
                        .collect();
                    self.dataset(cfg, &ni.node, port, &ni.partition, &own)
                })
                .collect()
        } else {
            vec![]
        };
        RunEvent {
            event_type: ty,
            event_time: at.to_rfc3339(),
            run: Run {
                run_id: uuid_from_ulid(ni.id.ulid()),
                facets: run_facets,
            },
            job: Job {
                namespace: cfg.namespace.clone(),
                name: format!("{}.{}", self.run.workflow, ni.node),
                facets: job_facets,
            },
            inputs,
            outputs,
            producer: cfg.producer.clone(),
            schema_url: RUN_EVENT_SCHEMA_URL.to_owned(),
        }
    }

    /// The run's OpenLineage events, in store order.
    ///
    /// # Errors
    ///
    /// [`LineageError::Inconsistent`] when an event or an output names a node instance of another run.
    pub fn run_events(&self, cfg: &LineageConfig) -> LineageResult<Vec<RunEvent>> {
        let by_id: HashMap<NodeInstanceId, &NodeInstance> =
            self.instances.iter().map(|ni| (ni.id, ni)).collect();
        if let Some(i) = self.instances.iter().find(|ni| ni.run != self.run.id) {
            return Err(LineageError::Inconsistent(format!(
                "node instance {} belongs to another run",
                i.id
            )));
        }
        if let Some(a) = self
            .outputs
            .iter()
            .find(|a| !by_id.contains_key(&a.node_instance))
        {
            return Err(LineageError::Inconsistent(format!(
                "output of unknown node instance {}",
                a.node_instance
            )));
        }
        let mut events: Vec<&Event> = self.events.iter().collect();
        events.sort_by_key(|e| e.seq);

        let created = events
            .iter()
            .find(|e| e.instance.is_none() && matches!(e.kind, EventKind::RunCreated { .. }))
            .map_or(self.run.created_at, |e| e.at);
        let mut out = vec![self.workflow_event(cfg, EventType::Start, created, RunState::Pending)];
        let mut last_at = created;
        let mut attempts: HashMap<NodeInstanceId, u32> = HashMap::new();
        let mut started: HashSet<NodeInstanceId> = HashSet::new();

        for e in events {
            if e.at > last_at {
                last_at = e.at;
            }
            let EventKind::Transition {
                from, cause, to, ..
            } = e.kind
            else {
                continue;
            };
            let Some(id) = e.instance else {
                continue;
            };
            let ni = by_id.get(&id).ok_or_else(|| {
                LineageError::Inconsistent(format!(
                    "event {} names unknown node instance {id}",
                    e.seq
                ))
            })?;
            let ty = match (from, cause, to) {
                (_, Cause::Dispatched, NodeState::Running) => {
                    let a = attempts.entry(id).or_insert(0);
                    *a += 1;
                    started.insert(id);
                    Some(if *a == 1 {
                        EventType::Start
                    } else {
                        EventType::Running
                    })
                }
                (_, _, NodeState::Running) => Some(EventType::Running),
                (NodeState::Running, _, NodeState::Succeeded) => Some(EventType::Complete),
                (_, _, NodeState::Failed) => Some(EventType::Fail),
                (_, _, NodeState::Cancelled) if started.contains(&id) => Some(EventType::Abort),
                (NodeState::Running, _, to) if !to.is_terminal() => Some(EventType::Running),
                _ => None,
            };
            if let Some(ty) = ty {
                let attempt = attempts.get(&id).copied().unwrap_or(0);
                out.push(self.node_event(cfg, &by_id, ni, ty, e.at, cause, to, attempt));
            }
        }

        let states: Vec<NodeState> = self.instances.iter().map(|ni| ni.state).collect();
        let state = derive_run_state(&states);
        let end = match state {
            RunState::Succeeded => Some(EventType::Complete),
            RunState::Failed => Some(EventType::Fail),
            RunState::Cancelled => Some(EventType::Abort),
            RunState::Pending | RunState::Running | RunState::Waiting => None,
        };
        if let Some(ty) = end {
            out.push(self.workflow_event(cfg, ty, last_at, state));
        }
        Ok(out)
    }
}

/// The OpenLineage view of one recorded agent run (O-05 section 6): job `bench.<benchmark>.<task>` and the
/// `ogun_agent_run` facet. Benchmark matrices export these beside their results.
#[must_use]
pub fn agent_run_event(record: &AgentRunRecord, cfg: &LineageConfig) -> RunEvent {
    use crate::records::AgentRunOutcome;
    let ty = match record.outcome {
        None => EventType::Start,
        Some(AgentRunOutcome::Completed) => EventType::Complete,
        Some(AgentRunOutcome::Cancelled) => EventType::Abort,
        Some(AgentRunOutcome::Failed | AgentRunOutcome::TimedOut | AgentRunOutcome::Error) => {
            EventType::Fail
        }
    };
    let at = record.finished_at.unwrap_or(record.started_at);
    let mut run_facets = Map::new();
    run_facets.insert(
        "ogun_agent_run".into(),
        ogun_facet(
            &cfg.producer,
            "OgunAgentRunFacet",
            serde_json::to_value(record).unwrap_or(Value::Null),
        ),
    );
    let mut job_facets = Map::new();
    job_facets.insert(
        "jobType".into(),
        facet(
            &cfg.producer,
            JOB_TYPE_SCHEMA,
            json!({ "processingType": "BATCH", "integration": "OGUN", "jobType": "AGENT_RUN" }),
        ),
    );
    RunEvent {
        event_type: ty,
        event_time: at.to_rfc3339(),
        run: Run {
            run_id: uuid_from_ulid(record.id.ulid()),
            facets: run_facets,
        },
        job: Job {
            namespace: cfg.namespace.clone(),
            name: format!("bench.{}.{}", record.task.benchmark, record.task.task_id),
            facets: job_facets,
        },
        inputs: vec![],
        outputs: vec![],
        producer: cfg.producer.clone(),
        schema_url: RUN_EVENT_SCHEMA_URL.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogun_run::Actor;
    use ogun_types::IdGenerator;

    fn ts(ms: i64) -> Timestamp {
        Timestamp::from_unix_millis(ms).unwrap()
    }

    #[test]
    fn uuid_has_v7_bits_and_keeps_millisecond_order() {
        // Maximal randomness in the earlier millisecond must still sort before minimal
        // randomness in the next millisecond. Same-millisecond ordering is not promised.
        let a = Ulid::from((1_000u128 << 80) | ((1u128 << 80) - 1));
        let b = Ulid::from(1_001u128 << 80);
        let ua = uuid_from_ulid(a);
        let ub = uuid_from_ulid(b);
        assert_eq!(ua.len(), 36);
        assert_eq!(&ua[14..15], "7");
        assert!(matches!(&ua[19..20], "8" | "9" | "a" | "b"));
        assert!(ua < ub);
    }

    #[test]
    fn a_two_node_run_maps_to_start_complete_with_versioned_datasets() {
        let ids = IdGenerator::new();
        let run = RunRecord {
            id: ids.run_id(),
            workflow: "climate".into(),
            version: 3,
            workflow_hash: "ab".repeat(32),
            partitions: vec!["2026-10-01".into()],
            created_at: ts(1_000),
        };
        let mk = |node: &str, state| NodeInstance {
            id: ids.node_instance_id(),
            run: run.id,
            node: node.into(),
            partition: "2026-10-01".into(),
            state,
            attempt: 1,
            last_cause: Some(Cause::OutputsCommitted),
            checkpoint: None,
            updated_at: ts(5_000),
        };
        let a = mk("fetch", NodeState::Succeeded);
        let b = mk("hash", NodeState::Succeeded);
        let mut seq = 0;
        let mut ev = |at, instance: Option<NodeInstanceId>, kind| {
            seq += 1;
            Event {
                seq,
                at: ts(at),
                run: run.id,
                instance,
                kind,
            }
        };
        let tr = |from, cause, to| EventKind::Transition {
            from,
            cause,
            to,
            actor: Actor::Runtime,
        };
        let events = vec![
            ev(1_000, None, EventKind::RunCreated { instances: 2 }),
            ev(
                1_100,
                Some(a.id),
                tr(NodeState::Pending, Cause::InputsAvailable, NodeState::Ready),
            ),
            ev(
                1_200,
                Some(a.id),
                tr(NodeState::Ready, Cause::Dispatched, NodeState::Running),
            ),
            ev(
                2_000,
                Some(a.id),
                tr(
                    NodeState::Running,
                    Cause::OutputsCommitted,
                    NodeState::Succeeded,
                ),
            ),
            ev(
                2_100,
                Some(b.id),
                tr(NodeState::Pending, Cause::InputsAvailable, NodeState::Ready),
            ),
            ev(
                2_200,
                Some(b.id),
                tr(NodeState::Ready, Cause::Dispatched, NodeState::Running),
            ),
            ev(
                3_000,
                Some(b.id),
                tr(
                    NodeState::Running,
                    Cause::OutputsCommitted,
                    NodeState::Succeeded,
                ),
            ),
        ];
        let out = OutputArtifact {
            node_instance: a.id,
            output: "artifacts".into(),
            artifact_id: None,
            blake3: "cd".repeat(32),
            sha256: None,
            size_bytes: 42,
            media_type: "text/csv".into(),
            committed_at: ts(1_900),
        };
        let lineage = RunLineage::new(run.clone(), vec![a, b], events)
            .with_outputs([out])
            .with_operator(
                "fetch",
                OperatorInfo {
                    op: "acquire@1".into(),
                    version: "1.0.0".into(),
                    manifest_blake3: None,
                    code_blake3: None,
                },
            );
        let mut lineage = lineage;
        lineage
            .node_inputs
            .insert("hash".into(), vec!["fetch.artifacts".into()]);
        let events = lineage.run_events(&LineageConfig::default()).unwrap();
        let types: Vec<EventType> = events.iter().map(|e| e.event_type).collect();
        assert_eq!(
            types,
            [
                EventType::Start,
                EventType::Start,
                EventType::Complete,
                EventType::Start,
                EventType::Complete,
                EventType::Complete
            ]
        );
        let fetch_done = &events[2];
        assert_eq!(fetch_done.job.name, "climate.fetch");
        assert_eq!(fetch_done.outputs.len(), 1);
        assert_eq!(fetch_done.outputs[0].name, "climate.fetch.artifacts");
        let version = fetch_done.outputs[0].facets["version"]["datasetVersion"]
            .as_str()
            .unwrap()
            .to_owned();
        let hash_start = &events[3];
        assert_eq!(
            hash_start.inputs[0].facets["version"]["datasetVersion"],
            version.as_str()
        );
        let text = serde_json::to_string(&events).unwrap();
        assert!(text.contains("\"eventType\":\"START\""));
        assert!(text.contains("\"schemaURL\""));
        // No floats anywhere: every event is hashable.
        for e in &events {
            zeus_verify::canonical::check_hashable(&serde_json::to_value(e).unwrap()).unwrap();
        }
    }
}
