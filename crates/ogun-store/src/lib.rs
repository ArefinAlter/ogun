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
                Some(_) => {}
                None => put(
                    &mut meta,
                    "store",
                    &Meta {
                        schema_version: SCHEMA_VERSION,
                        created_at: Timestamp::now(),
                        last_event_seq: 0,
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
                    index
                        .insert(format!("{}/{}", run.id, ni.id).as_str(), [].as_slice())
                        .map_err(storage)?;
                    count += 1;
                }
            }
        }
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
        let txn = self.write()?;
        let updated = Self::apply(&txn, id, cause, actor, now, checkpoint)?;
        txn.commit().map_err(storage)?;
        Ok(updated)
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
