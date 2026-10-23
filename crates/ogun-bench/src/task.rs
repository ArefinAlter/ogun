//! A benchmark task in adapter-neutral form (O-06 section 5).

use std::path::PathBuf;

use ogun_lineage::TaskRef;
use serde::{Deserialize, Serialize};

use crate::evaluator::EvaluatorSpec;

/// What the task needs from its environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEnvironment {
    /// A prebuilt image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// A build context with a `Dockerfile`, built once and tagged by its content digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_context: Option<PathBuf>,
    /// Working directory inside the container; the workspace is mounted here.
    pub workdir: String,
    /// Use the image's own working directory when it has one (Harbor images set `WORKDIR`).
    #[serde(default)]
    pub workdir_from_image: bool,
    /// The task declares it needs the network while the agent works (recorded; a person must still allow it).
    #[serde(default)]
    pub wants_network: bool,
    /// CPU limit the task asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// Memory limit the task asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    /// Build time limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_timeout_ms: Option<u64>,
}

/// One task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Benchmark family: `terminal_bench`, `spider2_dbt`, `ade_bench`.
    pub benchmark: String,
    /// Task id.
    pub id: String,
    /// Version, when the format has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Where the task came from.
    pub dir: PathBuf,
    /// The instruction given to the agent (untrusted text from the benchmark).
    pub instruction: String,
    /// Environment needs.
    pub environment: TaskEnvironment,
    /// Files or directories copied into the workspace before the agent starts, with the subdirectory they go to
    /// (empty: the workspace root).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixtures: Vec<(PathBuf, String)>,
    /// Files the agent may read, mounted read-only at `/benchmark`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark_files: Option<PathBuf>,
    /// Files staged at `/tests` only after the agent has finished (tests, gold answers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_files: Option<PathBuf>,
    /// A script run in the workspace before the agent starts (ADE-bench breaks the project this way). It is copied
    /// in, run and removed, so the agent does not read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_script: Option<PathBuf>,
    /// How the task is scored.
    pub evaluator: EvaluatorSpec,
    /// Agent time limit.
    pub agent_timeout_ms: u64,
    /// BLAKE3 over the task's files (instruction, fixtures, environment, tests).
    pub task_blake3: String,
}

impl Task {
    /// The reference agent-run records carry.
    #[must_use]
    pub fn task_ref(&self) -> TaskRef {
        TaskRef {
            benchmark: self.benchmark.clone(),
            task_id: self.id.clone(),
            task_version: self.version.clone(),
            task_blake3: self.task_blake3.clone(),
        }
    }
}
