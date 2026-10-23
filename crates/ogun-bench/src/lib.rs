//! The Ogun benchmark profile (O-06, ADR 0005).
//!
//! A benchmark is a controlled collection of agent runs: `Run = Workflow + Environment + Agent + Policy + Inputs +
//! Evaluator`. This crate provides the pieces and runs them:
//!
//! * [`env`]: the Environment (`env_`): a container (the `docker` or `podman` CLI) or a local temporary directory,
//!   with the network denied by default and a state directory of its own. A benchmark run never touches the user's
//!   Ogun or Zeus stores, `zeusd` or browser sessions: the environment refuses state directories inside theirs and
//!   passes no `OGUN_*` or `ZEUS_*` variable and no credential except the model credentials a person named.
//! * [`evaluator`]: the Evaluator (`evl_`): a command that runs in the environment after the agent and reports a
//!   result (a reward file, a JSON document or an exit code), converted to integer parts per million.
//! * [`agent`]: an agent run (`agr_`): the agent's command, with its trajectory recorded as JSON Lines (messages,
//!   tool calls, files touched, timings, token and cost totals; [`ogun_lineage::records`]).
//! * [`cache`]: result cache modes `off`, `read` and `read_write`, keyed by task, environment, agent and seed.
//! * [`adapters`]: Harbor task directories (Terminal-Bench 2.0), Spider 2.0-DBT and ADE-bench. Their formats are
//!   read as O-06 section 9 describes them; those descriptions are unverified against the upstream repositories.
//! * [`matrix`]: agents x tasks x seeds, with a results table in JSON and Markdown.
//!
//! The profile switches off product help (ADR 0005 decision 2): one attempt per run, no repair, no shared memory,
//! a fresh workspace for every run, and caches only when a person turns them on.

#![forbid(unsafe_code)]

pub mod adapters;
pub mod agent;
pub mod cache;
pub mod env;
pub mod evaluator;
pub mod matrix;
pub mod task;
pub mod workspace;

pub use agent::AgentSpec;
pub use cache::{ResultCache, ResultCacheMode};
pub use env::{CacheMode, Environment, EnvironmentSpec, NetworkPolicy, Runtime};
pub use evaluator::{EvalResult, EvaluatorSpec, ResultSource};
pub use matrix::{MatrixResults, MatrixSpec, run_matrix};
pub use task::Task;

/// Benchmark failures. Messages never contain secret values or model credentials.
#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    /// A task directory does not match its adapter's format.
    #[error("task {task} is not a valid {adapter} task: {reason}")]
    TaskInvalid {
        /// Adapter.
        adapter: &'static str,
        /// Task id or path.
        task: String,
        /// What is wrong.
        reason: String,
    },
    /// The task needs something this build does not do (for example a docker-compose environment).
    #[error("task {task} is not supported: {reason}")]
    TaskUnsupported {
        /// Task id.
        task: String,
        /// Why.
        reason: String,
    },
    /// The requested isolation cannot be enforced here; the run is refused rather than run with less.
    #[error("isolation unavailable: {0}")]
    IsolationUnavailable(String),
    /// The benchmark state directory overlaps the user's Ogun or Zeus state.
    #[error("the benchmark state directory must not overlap {0}")]
    StoreOverlap(&'static str),
    /// The container runtime or a local process failed.
    #[error("environment failed: {0}")]
    Environment(String),
    /// The evaluator did not produce a usable result.
    #[error("evaluator failed: {0}")]
    Evaluator(String),
    /// A matrix or agent specification is invalid.
    #[error("invalid specification: {0}")]
    Spec(String),
    /// File-system access failed.
    #[error("file access failed ({kind:?}) for {what}")]
    Io {
        /// What was being done.
        what: String,
        /// The error kind.
        kind: std::io::ErrorKind,
    },
    /// Recording failed.
    #[error(transparent)]
    Lineage(#[from] ogun_lineage::LineageError),
}

impl BenchError {
    /// The O-00 error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::TaskInvalid { .. } => "bench.task_invalid",
            Self::TaskUnsupported { .. } => "bench.task_unsupported",
            Self::IsolationUnavailable(_) | Self::StoreOverlap(_) => "bench.isolation_unavailable",
            Self::Environment(_) | Self::Io { .. } => "bench.environment_failed",
            Self::Evaluator(_) => "bench.evaluator_failed",
            Self::Spec(_) => "bench.spec_invalid",
            Self::Lineage(e) => e.code(),
        }
    }

    pub(crate) fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> Self {
        let what = what.into();
        move |e| Self::Io {
            what,
            kind: e.kind(),
        }
    }
}

/// Result alias.
pub type BenchResult<T> = Result<T, BenchError>;

/// BLAKE3 of the JCS form of a serialisable value, as 64 hex digits.
pub(crate) fn jcs_blake3<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| zeus_verify::canonical::to_jcs_bytes(&v).ok())
        .map_or_else(String::new, |b| blake3::hash(&b).to_hex().to_string())
}
