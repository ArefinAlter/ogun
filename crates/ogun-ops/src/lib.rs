//! Ogun operators (O-03).
//!
//! `ogund` hands one node instance to [`Registry::run`] as an [`Invocation`] and gets back an [`Outcome`]. The
//! registry decides how the operator runs: `core` operators in process, everything else as a sandboxed child
//! process speaking `jsonl-stdio/0` (O-03 section 4). The daemon owns everything around it: the run store, the
//! artifact store (outputs are ingested by the daemon after `Succeeded`), budgets, approvals and the session bridge.
//!
//! This module is the contract between the daemon and the operators; the operators themselves live in submodules.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use ogun_types::{NodeInstanceId, OgunError, RunId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

/// One input artifact, already committed in the artifact store and readable at `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputArtifact {
    /// The upstream reference this came through, `node_id.output`.
    pub port: String,
    /// Read-only path of the stored object.
    pub path: PathBuf,
    /// Content digest.
    pub blake3: String,
    /// Media type.
    pub media_type: String,
    /// Size.
    pub size_bytes: u64,
    /// Metadata the producing operator attached (predicates in routes read it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// One output an operator produced, a file inside its scratch directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    /// Output port name, as listed in the catalogue (O-03 section 7).
    pub port: String,
    /// Absolute path inside the scratch directory.
    pub path: PathBuf,
    /// Media type.
    pub media_type: String,
    /// Metadata for routing predicates and lineage (integers and strings only; no floats, O-02).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// The time window a partition covers, for operators that read by time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionWindow {
    /// Inclusive start, Unix ms.
    pub start_ms: i64,
    /// Exclusive end, Unix ms.
    pub end_ms: i64,
}

/// Everything an operator needs for one attempt.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// The run.
    pub run_id: RunId,
    /// The node instance.
    pub node_instance: NodeInstanceId,
    /// Node id within the workflow.
    pub node_id: String,
    /// Partition key, `""` when unpartitioned.
    pub partition: String,
    /// The partition's window, when the partitioning has one.
    pub window: Option<PartitionWindow>,
    /// Operator reference, `name@major`.
    pub op: String,
    /// The node's configuration in normal form. `{"secret_ref": "<name>"}` objects are unresolved; operators
    /// resolve them through [`Secrets`] and never write the values anywhere but a child's stdin.
    pub config: Value,
    /// Inputs.
    pub inputs: Vec<InputArtifact>,
    /// Fresh scratch directory for this attempt, writable.
    pub scratch: PathBuf,
    /// The last checkpoint, after `recovered`.
    pub checkpoint: Option<Value>,
    /// Limit for this attempt.
    pub timeout_ms: u64,
    /// The code file for `code` operators, already verified against `code.blake3` by the daemon.
    pub code: Option<CodeFile>,
    /// Attempt number, from 1.
    pub attempt: u32,
}

/// Verified code for a `code` operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFile {
    /// Content digest (equals the IR's `code.blake3`).
    pub blake3: String,
    /// Path of the verified bytes.
    pub path: PathBuf,
}

/// Resolves `secret_ref` names. The daemon's implementation reads its secret store.
pub trait Secrets: Send + Sync {
    /// The value, or `None` when no secret has this name.
    fn resolve(&self, name: &str) -> Option<Zeroizing<String>>;
}

/// A [`Secrets`] with nothing in it.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSecrets;

impl Secrets for NoSecrets {
    fn resolve(&self, _name: &str) -> Option<Zeroizing<String>> {
        None
    }
}

/// What an operator reports while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpEvent {
    /// Progress.
    Progress {
        /// Units done.
        done: u64,
        /// Units in total, when known.
        total: Option<u64>,
    },
    /// A checkpoint (at most 64 KiB of JSON) the daemon stores and gives back after a restart.
    Checkpoint(Value),
    /// Model use, for the spend ledger.
    Usage {
        /// Provider name from the ladder.
        provider: String,
        /// Model id.
        model: String,
        /// Input tokens.
        input_tokens: u64,
        /// Output tokens.
        output_tokens: u64,
        /// Cost in micro-US-dollars.
        cost_microusd: u64,
    },
    /// A diagnostic line (already redacted), for the run log.
    Log(String),
}

/// What the daemon gives every attempt besides the invocation.
#[derive(Clone)]
pub struct Context {
    /// Secret resolution.
    pub secrets: Arc<dyn Secrets>,
    /// Progress, checkpoints, usage and logs.
    pub events: mpsc::UnboundedSender<OpEvent>,
    /// Becomes `true` when the attempt is cancelled; operators stop promptly and return `Cancelled`.
    pub cancel: watch::Receiver<bool>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context").finish_non_exhaustive()
    }
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Outputs are ready in scratch; the daemon ingests and commits them.
    Succeeded {
        /// Outputs.
        outputs: Vec<OutputFile>,
    },
    /// Failed. `retryable` decides between `retryable_error` and `permanent_error` (O-03 section 5).
    Failed {
        /// The envelope; never contains secrets, URLs with query strings or configuration values.
        error: OgunError,
        /// Retry if retries remain.
        retryable: bool,
    },
    /// The operator needs a browser-session source through `zeusd` (O-08). The daemon submits the request on the
    /// session bridge and re-dispatches when Zeus reports the job's outcome.
    SessionRequired {
        /// The Zeus job request, as O-08 describes it.
        request: Value,
    },
    /// The attempt stopped because it was cancelled.
    Cancelled,
}

/// Where operators come from and how they may run. Built once by the daemon from its configuration.
#[derive(Debug, Clone)]
pub struct Environment {
    /// The daemon's state directory.
    pub state_dir: PathBuf,
    /// Directories searched for operator manifests (`<name>.manifest.json`) and their binaries.
    pub operator_dirs: Vec<PathBuf>,
    /// Whether `model` operators may run (constraint 6: a workflow without `llm.*` runs with no model access).
    pub model_access: bool,
    /// Whether child operators may run without an OS sandbox when none is available. Off by default; on only for
    /// development profiles, and then every such attempt is logged.
    pub allow_unsandboxed: bool,
}

/// The operator registry.
#[derive(Debug, Clone)]
pub struct Registry {
    env: Arc<Environment>,
}

impl Registry {
    /// A registry for this environment.
    #[must_use]
    pub fn new(env: Environment) -> Self {
        Self { env: Arc::new(env) }
    }

    /// The environment.
    #[must_use]
    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// Whether this build can run the operator.
    #[must_use]
    pub fn knows(&self, op: &str) -> bool {
        let _ = op;
        false
    }

    /// Runs one attempt.
    pub async fn run(&self, inv: Invocation, ctx: Context) -> Outcome {
        let _ = ctx;
        Outcome::Failed {
            error: OgunError::new("op.unknown", format!("no implementation for {}", inv.op)),
            retryable: false,
        }
    }
}
