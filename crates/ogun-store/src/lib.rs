//! The durable Ogun run store (O-01 section 4).
//!
//! One redb database per `ogund`, opened by exactly one process. Every state change of a node
//! instance and the event that records it commit in one write transaction with
//! `Durability::Immediate`, so after a crash the store holds either both or neither. On startup
//! [`Store::recover`] moves every instance that was `running` back to `ready` (cause `recovered`),
//! which is how a killed backfill resumes only its unfinished partitions.
//!
//! Rows are versioned JSON, like the zeus-core journal. Values copied from workflow configuration
//! are never written to events; only states, causes and identifiers are.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::Path;

use ogun_ir::Workflow;
use ogun_ir::hash::{canonical_bytes, workflow_hash};
use ogun_run::{Actor, Cause, NodeState, RunState, derive_run_state, transition};
use ogun_types::{IdGenerator, NodeInstanceId, RunId};
use redb::{Durability, ReadableDatabase, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeus_types::Timestamp;

/// Schema version of the rows below. Opening a newer store fails rather than guessing.
pub const SCHEMA_VERSION: u32 = 1;

/// Largest checkpoint an operator may store, in bytes (O-03 section 4).
pub const MAX_CHECKPOINT_BYTES: usize = 64 * 1024;

type Table = TableDefinition<'static, &'static str, &'static [u8]>;

const META: Table = TableDefinition::new("meta");
const WORKFLOWS: Table = TableDefinition::new("workflows");
const RUNS: Table = TableDefinition::new("runs");
const INSTANCES: Table = TableDefinition::new("instances");
const RUN_INSTANCES: Table = TableDefinition::new("run_instances");
const EVENTS: Table = TableDefinition::new("events");
const DONE: Table = TableDefinition::new("partitions_done");
const DOCS: Table = TableDefinition::new("docs");
const ACTIVE: Table = TableDefinition::new("active");

/// Daemon-owned documents kept beside the run state, in the same database and with the same
/// durability, so a state change and the facts that go with it commit together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Collection {
    /// Workflow registry rows, by name.
    Workflows,
    /// Stored workflow versions, by `name/version` (zero-padded).
    Versions,
    /// Approvals, by approval id.
    Approvals,
    /// Idempotency records, by `credential/method/key`.
    Idempotency,
    /// Committed outputs of a node instance, by instance id.
    Outputs,
    /// Retry deadlines, by instance id.
    Retries,
    /// WebAuthn approver keys, by credential id.
    Approvers,
    /// Enrolled edges, by edge id.
    Edges,
    /// Stream state: window revisions and producer positions.
    Streams,
    /// Code hashes a person approved, by `workflow/blake3`.
    CodeApprovals,
    /// Model spend, by run id.
    Spend,
    /// Session-bridge requests, by instance id.
    Sessions,
    /// Verified code bundles known to the daemon, by BLAKE3.
    Code,
    /// Run-time approvals granted to a node instance, by instance id.
    Granted,
    /// The last error of a node instance (an O-00 envelope), by instance id.
    Errors,
}

impl Collection {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Workflows => "wf/",
            Self::Versions => "ver/",
            Self::Approvals => "ap/",
            Self::Idempotency => "idem/",
            Self::Outputs => "out/",
            Self::Retries => "retry/",
            Self::Approvers => "apr/",
            Self::Edges => "edge/",
            Self::Streams => "stream/",
            Self::CodeApprovals => "codeok/",
            Self::Spend => "spend/",
            Self::Sessions => "sess/",
            Self::Code => "code/",
            Self::Granted => "granted/",
            Self::Errors => "err/",
        }
    }

    fn key(self, key: &str) -> String {
        format!("{}{key}", self.prefix())
    }
}

/// One document write: `value: None` deletes.
#[derive(Debug, Clone, PartialEq)]
pub struct DocWrite {
    /// Collection.
    pub collection: Collection,
    /// Key within the collection.
    pub key: String,
    /// New value, or `None` to delete.
    pub value: Option<Value>,
}

impl DocWrite {
    /// A put.
    ///
    /// # Errors
    ///
    /// A storage error when the value cannot be encoded.
    pub fn put(
        collection: Collection,
        key: impl Into<String>,
        value: &impl Serialize,
    ) -> StoreResult<Self> {
        Ok(Self {
            collection,
            key: key.into(),
            value: Some(serde_json::to_value(value).map_err(storage)?),
        })
    }

    /// A delete.
    #[must_use]
    pub fn delete(collection: Collection, key: impl Into<String>) -> Self {
        Self {
            collection,
            key: key.into(),
            value: None,
        }
    }
}

/// Store failures.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Another process holds the store.
    #[error("the run store is already open in another process")]
    AlreadyOpen,
    /// The store was written by a newer schema.
    #[error("run store schema {found} is newer than this build ({SCHEMA_VERSION})")]
    VersionUnsupported {
        /// The schema found on disk.
        found: u32,
    },
    /// The database failed.
    #[error("storage: {0}")]
    Storage(String),
    /// A row could not be decoded.
    #[error("corrupt row in {table}")]
    Corrupt {
        /// The table.
        table: &'static str,
    },
    /// No such run or node instance.
    #[error("{0} not found")]
    NotFound(String),
    /// The O-01 table does not allow this transition.
    #[error(transparent)]
    InvalidTransition(#[from] ogun_run::InvalidTransition),
    /// A checkpoint exceeds [`MAX_CHECKPOINT_BYTES`].
    #[error("checkpoint larger than 64 KiB")]
    CheckpointTooLarge,
    /// The workflow cannot be hashed.
    #[error("workflow cannot be hashed")]
    Unhashable,
}

impl StoreError {
    /// The O-00 error code for this failure.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "run.not_found",
            Self::InvalidTransition(_) => "run.invalid_transition",
            Self::CheckpointTooLarge | Self::Unhashable => "ir.invalid",
            _ => "internal.bug",
        }
    }
}

fn storage(e: impl std::fmt::Display) -> StoreError {
    StoreError::Storage(e.to_string())
}

/// Result alias.
pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    schema_version: u32,
    created_at: Timestamp,
    last_event_seq: u64,
    /// The `active` index exists (stores created before it are indexed on open).
    #[serde(default)]
    active_index: bool,
}

/// A stored run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// The run.
    pub id: RunId,
    /// Workflow name.
    pub workflow: String,
    /// Workflow version number.
    pub version: u64,
    /// The hash the run is bound to for its whole life.
    pub workflow_hash: String,
    /// Partition keys covered; `[""]` for an unpartitioned workflow.
    pub partitions: Vec<String>,
    /// When the run was created.
    pub created_at: Timestamp,
}

/// One node of one run for one partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInstance {
    /// The instance.
    pub id: NodeInstanceId,
    /// Its run.
    pub run: RunId,
    /// Node id within the workflow.
    pub node: String,
    /// Partition key; `""` when unpartitioned.
    pub partition: String,
    /// Current state.
    pub state: NodeState,
    /// Attempts started (incremented on `dispatched`).
    pub attempt: u32,
    /// The cause of the last transition; tells a skip by denial from a skip as not needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cause: Option<Cause>,
    /// The operator's last checkpoint, given back after `recovered`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<Value>,
    /// Last change.
    pub updated_at: Timestamp,
}

/// One recorded transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Store-wide sequence number, from 1, without gaps.
    pub seq: u64,
    /// When it was committed.
    pub at: Timestamp,
    /// The run.
    pub run: RunId,
    /// The node instance; absent for run-level events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<NodeInstanceId>,
    /// What happened.
    pub kind: EventKind,
}

/// Event payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// The run was created with this many node instances.
    RunCreated {
        /// Node instances created.
        instances: u32,
    },
    /// A node instance changed state.
    Transition {
        /// Previous state.
        from: NodeState,
        /// Cause.
        cause: Cause,
        /// New state.
        to: NodeState,
        /// Who caused it.
        actor: Actor,
    },
}

/// The run store.
#[derive(Debug)]
pub struct Store {
    db: redb::Database,
}

fn get<T: DeserializeOwned>(
    t: &impl ReadableTable<&'static str, &'static [u8]>,
    name: &'static str,
    key: &str,
) -> StoreResult<Option<T>> {
    match t.get(key).map_err(storage)? {
        None => Ok(None),
        Some(v) => serde_json::from_slice(v.value())
            .map(Some)
            .map_err(|_| StoreError::Corrupt { table: name }),
    }
}

fn put<T: Serialize>(
    t: &mut redb::Table<&'static str, &'static [u8]>,
    key: &str,
    value: &T,
) -> StoreResult<()> {
    let bytes = serde_json::to_vec(value).map_err(storage)?;
    t.insert(key, bytes.as_slice()).map_err(storage)?;
    Ok(())
}

/// Succeeded, or skipped for a reason other than a person's denial. A partition is done only when
/// every instance is finished this way: a denied node means its outputs were never produced.
fn finished_ok(ni: &NodeInstance) -> bool {
    match ni.state {
        NodeState::Succeeded => true,
        NodeState::Skipped => ni.last_cause != Some(Cause::ApprovalDenied),
        _ => false,
    }
}

fn event_key(seq: u64) -> String {
    format!("{seq:020}")
}

fn done_key(hash: &str, partition: &str) -> String {
    format!("{hash}/{partition}")
}

impl Store {
    /// Opens or creates the store at `path`.
    ///
    /// # Errors
    ///
    /// [`StoreError::AlreadyOpen`] when another process holds it, [`StoreError::VersionUnsupported`]
    /// for a newer schema, [`StoreError::Storage`] otherwise.
    pub fn open(path: &Path) -> StoreResult<Self> {
        let db = redb::Database::create(path).map_err(|e| match e {
            redb::DatabaseError::DatabaseAlreadyOpen => StoreError::AlreadyOpen,
            other => storage(other),
        })?;
        let store = Self { db };
        let txn = store.write()?;
        {
            for t in [
                META,
                WORKFLOWS,
                RUNS,
                INSTANCES,
                RUN_INSTANCES,
                EVENTS,
                DONE,
                DOCS,
                ACTIVE,
            ] {
                txn.open_table(t).map_err(storage)?;
            }
            let mut meta = txn.open_table(META).map_err(storage)?;
            match get::<Meta>(&meta, "meta", "store")? {
                Some(m) if m.schema_version > SCHEMA_VERSION => {
                    return Err(StoreError::VersionUnsupported {
                        found: m.schema_version,
                    });
                }
                Some(mut m) if !m.active_index => {
                    let instances = txn.open_table(INSTANCES).map_err(storage)?;
                    let mut active = txn.open_table(ACTIVE).map_err(storage)?;
                    for row in instances.iter().map_err(storage)? {
                        let (k, v) = row.map_err(storage)?;
                        let ni: NodeInstance = serde_json::from_slice(v.value())
                            .map_err(|_| StoreError::Corrupt { table: "instances" })?;
                        if !ni.state.is_terminal() {
                            active
                                .insert(k.value(), ni.run.to_string().as_bytes())
                                .map_err(storage)?;
                        }
                    }
                    m.active_index = true;
                    put(&mut meta, "store", &m)?;
                }
                Some(_) => {}
                None => put(
                    &mut meta,
                    "store",
                    &Meta {
                        schema_version: SCHEMA_VERSION,
                        created_at: Timestamp::now(),
                        last_event_seq: 0,
                        active_index: true,
                    },
                )?,
            }
        }
        txn.commit().map_err(storage)?;
        Ok(store)
    }

    fn write(&self) -> StoreResult<redb::WriteTransaction> {
        let mut txn = self.db.begin_write().map_err(storage)?;
        txn.set_durability(Durability::Immediate).map_err(storage)?;
        Ok(txn)
    }

    fn append(
        txn: &redb::WriteTransaction,
        at: Timestamp,
        run: RunId,
        instance: Option<NodeInstanceId>,
        kind: EventKind,
    ) -> StoreResult<u64> {
        let mut meta_t = txn.open_table(META).map_err(storage)?;
        let mut meta: Meta =
            get(&meta_t, "meta", "store")?.ok_or(StoreError::Corrupt { table: "meta" })?;
        meta.last_event_seq += 1;
        let seq = meta.last_event_seq;
        put(&mut meta_t, "store", &meta)?;
        let mut events = txn.open_table(EVENTS).map_err(storage)?;
        put(
            &mut events,
            &event_key(seq),
            &Event {
                seq,
                at,
                run,
                instance,
                kind,
            },
        )?;
        Ok(seq)
    }

    /// Creates a run of `wf` over `partitions` (use `[""]` for an unpartitioned workflow), with
    /// one `pending` node instance per node and partition. Stores the workflow's canonical form
    /// under its hash.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unhashable`] or a storage error.
    pub fn create_run(
        &self,
        wf: &Workflow,
        partitions: &[String],
        ids: &IdGenerator,
        now: Timestamp,
    ) -> StoreResult<RunRecord> {
        self.create_run_with(wf, partitions, ids, now, |_| Ok(Vec::new()))
    }

    /// As [`Store::create_run`], and commits the documents `docs` returns for the new run in the
    /// same transaction (an idempotency record, for example).
    ///
    /// # Errors
    ///
    /// As [`Store::create_run`], or the error `docs` returns.
    pub fn create_run_with(
        &self,
        wf: &Workflow,
        partitions: &[String],
        ids: &IdGenerator,
        now: Timestamp,
        docs: impl FnOnce(&RunRecord) -> StoreResult<Vec<DocWrite>>,
    ) -> StoreResult<RunRecord> {
        let hash = workflow_hash(wf)
            .map_err(|_| StoreError::Unhashable)?
            .to_string();
        let canonical = canonical_bytes(wf).map_err(|_| StoreError::Unhashable)?;
        let partitions: Vec<String> = if partitions.is_empty() {
            vec![String::new()]
        } else {
            partitions.to_vec()
        };
        let run = RunRecord {
            id: ids.run_id(),
            workflow: wf.workflow.clone(),
            version: wf.version,
            workflow_hash: hash.clone(),
            partitions: partitions.clone(),
            created_at: now,
        };
        let txn = self.write()?;
        let mut count = 0u32;
        {
            txn.open_table(WORKFLOWS)
                .map_err(storage)?
                .insert(hash.as_str(), canonical.as_slice())
                .map_err(storage)?;
            put(
                &mut txn.open_table(RUNS).map_err(storage)?,
                &run.id.to_string(),
                &run,
            )?;
            let mut instances = txn.open_table(INSTANCES).map_err(storage)?;
            let mut index = txn.open_table(RUN_INSTANCES).map_err(storage)?;
            let mut active = txn.open_table(ACTIVE).map_err(storage)?;
            let run_key = run.id.to_string();
            for partition in &partitions {
                for node in &wf.nodes {
                    let ni = NodeInstance {
                        id: ids.node_instance_id(),
                        run: run.id,
                        node: node.id.clone(),
                        partition: partition.clone(),
                        state: NodeState::Pending,
                        attempt: 0,
                        last_cause: None,
                        checkpoint: None,
                        updated_at: now,
                    };
                    put(&mut instances, &ni.id.to_string(), &ni)?;
                    active
                        .insert(ni.id.to_string().as_str(), run_key.as_bytes())
                        .map_err(storage)?;
                    index
                        .insert(format!("{}/{}", run.id, ni.id).as_str(), [].as_slice())
                        .map_err(storage)?;
                    count += 1;
                }
            }
        }
        Self::write_docs_in(&txn, &docs(&run)?)?;
        Self::append(
            &txn,
            now,
            run.id,
            None,
            EventKind::RunCreated { instances: count },
        )?;
        txn.commit().map_err(storage)?;
        Ok(run)
    }

    /// Stores a workflow's canonical form under its hash, with `docs`, in one transaction. Returns
    /// the hash. Runs store their workflow too; this is for versions stored before any run.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unhashable`] or a storage error.
    pub fn put_workflow(&self, wf: &Workflow, docs: &[DocWrite]) -> StoreResult<String> {
        let hash = workflow_hash(wf)
            .map_err(|_| StoreError::Unhashable)?
            .to_string();
        let canonical = canonical_bytes(wf).map_err(|_| StoreError::Unhashable)?;
        let txn = self.write()?;
        txn.open_table(WORKFLOWS)
            .map_err(storage)?
            .insert(hash.as_str(), canonical.as_slice())
            .map_err(storage)?;
        Self::write_docs_in(&txn, docs)?;
        txn.commit().map_err(storage)?;
        Ok(hash)
    }

    fn write_docs_in(txn: &redb::WriteTransaction, writes: &[DocWrite]) -> StoreResult<()> {
        if writes.is_empty() {
            return Ok(());
        }
        let mut t = txn.open_table(DOCS).map_err(storage)?;
        for w in writes {
            let key = w.collection.key(&w.key);
            match &w.value {
                Some(v) => put(&mut t, &key, v)?,
                None => {
                    t.remove(key.as_str()).map_err(storage)?;
                }
            }
        }
        Ok(())
    }

    /// Writes documents in one transaction.
    ///
    /// # Errors
    ///
    /// A storage error; nothing is written then.
    pub fn write_docs(&self, writes: &[DocWrite]) -> StoreResult<()> {
        let txn = self.write()?;
        Self::write_docs_in(&txn, writes)?;
        txn.commit().map_err(storage)
    }

    /// Stores one document.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn put_doc(
        &self,
        collection: Collection,
        key: &str,
        value: &impl Serialize,
    ) -> StoreResult<()> {
        self.write_docs(&[DocWrite::put(collection, key, value)?])
    }

    /// Deletes one document (absent is fine).
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn delete_doc(&self, collection: Collection, key: &str) -> StoreResult<()> {
        self.write_docs(&[DocWrite::delete(collection, key)])
    }

    /// One document.
    ///
    /// # Errors
    ///
    /// A storage error, or [`StoreError::Corrupt`] when it does not decode as `T`.
    pub fn get_doc<T: DeserializeOwned>(
        &self,
        collection: Collection,
        key: &str,
    ) -> StoreResult<Option<T>> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(DOCS).map_err(storage)?;
        get(&t, "docs", &collection.key(key))
    }

    /// Every document in a collection whose key starts with `prefix`, in key order.
    ///
    /// # Errors
    ///
    /// A storage error, or [`StoreError::Corrupt`] when one does not decode as `T`.
    pub fn list_docs<T: DeserializeOwned>(
        &self,
        collection: Collection,
        prefix: &str,
    ) -> StoreResult<Vec<(String, T)>> {
        let full = collection.key(prefix);
        let strip = collection.prefix().len();
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(DOCS).map_err(storage)?;
        let mut out = Vec::new();
        for row in t.range(full.as_str()..).map_err(storage)? {
            let (k, v) = row.map_err(storage)?;
            let key = k.value();
            if !key.starts_with(&full) {
                break;
            }
            let value = serde_json::from_slice(v.value())
                .map_err(|_| StoreError::Corrupt { table: "docs" })?;
            out.push((key[strip..].to_owned(), value));
        }
        Ok(out)
    }

    /// Applies one O-01 transition to a node instance, records the event and, when the last
    /// instance of a partition finishes, marks the partition done for the run's workflow hash.
    /// `checkpoint` replaces the stored checkpoint when given.
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidTransition`] when the table forbids it (nothing is written),
    /// [`StoreError::NotFound`], [`StoreError::CheckpointTooLarge`] or a storage error.
    pub fn transition(
        &self,
        id: NodeInstanceId,
        cause: Cause,
        actor: Actor,
        now: Timestamp,
        checkpoint: Option<Value>,
    ) -> StoreResult<NodeInstance> {
        if let Some(c) = &checkpoint
            && serde_json::to_vec(c).map_err(storage)?.len() > MAX_CHECKPOINT_BYTES
        {
            return Err(StoreError::CheckpointTooLarge);
        }
        self.transition_with(id, cause, actor, now, checkpoint, &[])
    }

    /// As [`Store::transition`], and writes `docs` in the same transaction: an operator's outputs
    /// commit with `outputs_committed`, an approval's decision with `approval_granted`.
    ///
    /// # Errors
    ///
    /// As [`Store::transition`]; nothing is written on error.
    pub fn transition_with(
        &self,
        id: NodeInstanceId,
        cause: Cause,
        actor: Actor,
        now: Timestamp,
        checkpoint: Option<Value>,
        docs: &[DocWrite],
    ) -> StoreResult<NodeInstance> {
        if let Some(c) = &checkpoint
            && serde_json::to_vec(c).map_err(storage)?.len() > MAX_CHECKPOINT_BYTES
        {
            return Err(StoreError::CheckpointTooLarge);
        }
        let txn = self.write()?;
        let updated = Self::apply(&txn, id, cause, actor, now, checkpoint)?;
        Self::write_docs_in(&txn, docs)?;
        txn.commit().map_err(storage)?;
        Ok(updated)
    }

    /// Replaces an instance's checkpoint without a transition (operators checkpoint while running).
    ///
    /// # Errors
    ///
    /// [`StoreError::CheckpointTooLarge`], [`StoreError::NotFound`] or a storage error.
    pub fn set_checkpoint(&self, id: NodeInstanceId, checkpoint: Value) -> StoreResult<()> {
        if serde_json::to_vec(&checkpoint).map_err(storage)?.len() > MAX_CHECKPOINT_BYTES {
            return Err(StoreError::CheckpointTooLarge);
        }
        let key = id.to_string();
        let txn = self.write()?;
        {
            let mut t = txn.open_table(INSTANCES).map_err(storage)?;
            let mut ni: NodeInstance =
                get(&t, "instances", &key)?.ok_or_else(|| StoreError::NotFound(key.clone()))?;
            ni.checkpoint = Some(checkpoint);
            put(&mut t, &key, &ni)?;
        }
        txn.commit().map_err(storage)
    }

    /// One node instance.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] or a storage error.
    pub fn instance(&self, id: NodeInstanceId) -> StoreResult<NodeInstance> {
        let key = id.to_string();
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(INSTANCES).map_err(storage)?;
        get(&t, "instances", &key)?.ok_or(StoreError::NotFound(key))
    }

    /// Every node instance not in a terminal state, in id (creation) order.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn active_instances(&self) -> StoreResult<Vec<NodeInstance>> {
        let txn = self.db.begin_read().map_err(storage)?;
        let active = txn.open_table(ACTIVE).map_err(storage)?;
        let t = txn.open_table(INSTANCES).map_err(storage)?;
        let mut out = Vec::new();
        for row in active.iter().map_err(storage)? {
            let (k, _) = row.map_err(storage)?;
            if let Some(ni) = get::<NodeInstance>(&t, "instances", k.value())? {
                out.push(ni);
            }
        }
        Ok(out)
    }

    /// Runs newest first, created before `before` when given, at most `limit`.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn list_runs(&self, before: Option<RunId>, limit: usize) -> StoreResult<Vec<RunRecord>> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(RUNS).map_err(storage)?;
        let mut out = Vec::new();
        let rows: Box<dyn Iterator<Item = _>> = match before {
            Some(b) => {
                let end = b.to_string();
                Box::new(t.range(..end.as_str()).map_err(storage)?.rev())
            }
            None => Box::new(t.iter().map_err(storage)?.rev()),
        };
        for row in rows.take(limit) {
            let (_, v) = row.map_err(storage)?;
            out.push(
                serde_json::from_slice(v.value())
                    .map_err(|_| StoreError::Corrupt { table: "runs" })?,
            );
        }
        Ok(out)
    }

    /// The highest event sequence number.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn last_event_seq(&self) -> StoreResult<u64> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(META).map_err(storage)?;
        Ok(get::<Meta>(&t, "meta", "store")?.map_or(0, |m| m.last_event_seq))
    }

    fn apply(
        txn: &redb::WriteTransaction,
        id: NodeInstanceId,
        cause: Cause,
        actor: Actor,
        now: Timestamp,
        checkpoint: Option<Value>,
    ) -> StoreResult<NodeInstance> {
        let key = id.to_string();
        let mut ni: NodeInstance = {
            let t = txn.open_table(INSTANCES).map_err(storage)?;
            get(&t, "instances", &key)?.ok_or_else(|| StoreError::NotFound(key.clone()))?
        };
        let from = ni.state;
        let to = transition(from, cause, actor)?;
        ni.state = to;
        ni.last_cause = Some(cause);
        ni.updated_at = now;
        if cause == Cause::Dispatched {
            ni.attempt += 1;
        }
        if checkpoint.is_some() {
            ni.checkpoint = checkpoint;
        }
        put(&mut txn.open_table(INSTANCES).map_err(storage)?, &key, &ni)?;
        if to.is_terminal() {
            txn.open_table(ACTIVE)
                .map_err(storage)?
                .remove(key.as_str())
                .map_err(storage)?;
        }
        Self::append(
            txn,
            now,
            ni.run,
            Some(id),
            EventKind::Transition {
                from,
                cause,
                to,
                actor,
            },
        )?;
        if finished_ok(&ni) {
            Self::mark_partition_if_done(txn, &ni)?;
        }
        Ok(ni)
    }

    fn mark_partition_if_done(txn: &redb::WriteTransaction, ni: &NodeInstance) -> StoreResult<()> {
        let run: RunRecord = {
            let t = txn.open_table(RUNS).map_err(storage)?;
            get(&t, "runs", &ni.run.to_string())?
                .ok_or_else(|| StoreError::NotFound(ni.run.to_string()))?
        };
        let siblings = Self::instances_in(txn, ni.run)?;
        let finished = siblings
            .iter()
            .filter(|s| s.partition == ni.partition)
            .all(finished_ok);
        if finished {
            txn.open_table(DONE)
                .map_err(storage)?
                .insert(
                    done_key(&run.workflow_hash, &ni.partition).as_str(),
                    [].as_slice(),
                )
                .map_err(storage)?;
        }
        Ok(())
    }

    fn instances_in(txn: &redb::WriteTransaction, run: RunId) -> StoreResult<Vec<NodeInstance>> {
        let prefix = format!("{run}/");
        let index = txn.open_table(RUN_INSTANCES).map_err(storage)?;
        let table = txn.open_table(INSTANCES).map_err(storage)?;
        let mut out = Vec::new();
        for row in index.range(prefix.as_str()..).map_err(storage)? {
            let (k, _) = row.map_err(storage)?;
            let Some(ni) = k.value().strip_prefix(&prefix) else {
                break;
            };
            if let Some(i) = get(&table, "instances", ni)? {
                out.push(i);
            }
        }
        Ok(out)
    }

    /// Startup recovery: every `running` instance moves to `ready` with cause `recovered`, in one
    /// transaction. Returns the instances it moved. Their checkpoints are kept.
    ///
    /// # Errors
    ///
    /// A storage error; nothing is changed then.
    pub fn recover(&self, now: Timestamp) -> StoreResult<Vec<NodeInstanceId>> {
        let running: Vec<NodeInstanceId> = {
            let txn = self.db.begin_read().map_err(storage)?;
            let t = txn.open_table(INSTANCES).map_err(storage)?;
            let mut ids = Vec::new();
            for row in t.iter().map_err(storage)? {
                let (_, v) = row.map_err(storage)?;
                let ni: NodeInstance = serde_json::from_slice(v.value())
                    .map_err(|_| StoreError::Corrupt { table: "instances" })?;
                if ni.state == NodeState::Running {
                    ids.push(ni.id);
                }
            }
            ids
        };
        if running.is_empty() {
            return Ok(running);
        }
        let txn = self.write()?;
        for id in &running {
            Self::apply(&txn, *id, Cause::Recovered, Actor::Runtime, now, None)?;
        }
        txn.commit().map_err(storage)?;
        Ok(running)
    }

    /// A run.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] or a storage error.
    pub fn run(&self, id: RunId) -> StoreResult<RunRecord> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(RUNS).map_err(storage)?;
        get(&t, "runs", &id.to_string())?.ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    /// The node instances of a run, in creation order.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn instances(&self, run: RunId) -> StoreResult<Vec<NodeInstance>> {
        let prefix = format!("{run}/");
        let txn = self.db.begin_read().map_err(storage)?;
        let index = txn.open_table(RUN_INSTANCES).map_err(storage)?;
        let table = txn.open_table(INSTANCES).map_err(storage)?;
        let mut out = Vec::new();
        for row in index.range(prefix.as_str()..).map_err(storage)? {
            let (k, _) = row.map_err(storage)?;
            let Some(ni) = k.value().strip_prefix(&prefix) else {
                break;
            };
            if let Some(i) = get(&table, "instances", ni)? {
                out.push(i);
            }
        }
        Ok(out)
    }

    /// The run's state, derived from its instances (O-01 section 5.3).
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn run_state(&self, run: RunId) -> StoreResult<RunState> {
        let states: Vec<NodeState> = self.instances(run)?.iter().map(|i| i.state).collect();
        Ok(derive_run_state(&states))
    }

    /// Every event with a sequence number greater than `after`, in order, at most `limit`.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn events_after(&self, after: u64, limit: usize) -> StoreResult<Vec<Event>> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(EVENTS).map_err(storage)?;
        let start = event_key(after.saturating_add(1));
        let mut out = Vec::new();
        for row in t.range(start.as_str()..).map_err(storage)?.take(limit) {
            let (_, v) = row.map_err(storage)?;
            out.push(
                serde_json::from_slice(v.value())
                    .map_err(|_| StoreError::Corrupt { table: "events" })?,
            );
        }
        Ok(out)
    }

    /// Partitions finished (every node succeeded or was skipped) under `workflow_hash`, in any run.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn done_partitions(&self, workflow_hash: &str) -> StoreResult<BTreeSet<String>> {
        let prefix = format!("{workflow_hash}/");
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(DONE).map_err(storage)?;
        let mut out = BTreeSet::new();
        for row in t.range(prefix.as_str()..).map_err(storage)? {
            let (k, _) = row.map_err(storage)?;
            let Some(p) = k.value().strip_prefix(&prefix) else {
                break;
            };
            out.insert(p.to_owned());
        }
        Ok(out)
    }

    /// The canonical workflow document stored under a hash.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn workflow_bytes(&self, workflow_hash: &str) -> StoreResult<Option<Vec<u8>>> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(WORKFLOWS).map_err(storage)?;
        Ok(t.get(workflow_hash)
            .map_err(storage)?
            .map(|v| v.value().to_vec()))
    }
}
