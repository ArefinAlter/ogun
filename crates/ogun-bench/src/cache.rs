//! The result cache (O-06 section 8): reuse a finished agent run instead of repeating it.
//!
//! The key is the BLAKE3 of the JCS form of `{task_blake3, environment_blake3, agent_config_blake3, seed,
//! evaluator}`: a result is reused only for the same task content, the same environment (image digest, fixture,
//! limits, network policy, cache mode), the same agent configuration, the same seed and the same evaluator. The
//! cache lives under the benchmark state directory, never in a user's store.

use std::path::{Path, PathBuf};

use ogun_lineage::AgentRunRecord;
use serde::{Deserialize, Serialize};

use crate::{BenchError, BenchResult, jcs_blake3};

/// How the result cache is used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultCacheMode {
    /// Every run executes. The default: benchmark mode measures, it does not reuse.
    #[default]
    Off,
    /// Reuse cached results; record nothing new.
    Read,
    /// Reuse cached results and store new ones.
    ReadWrite,
}

/// The cache directory and its mode.
#[derive(Debug, Clone)]
pub struct ResultCache {
    mode: ResultCacheMode,
    dir: PathBuf,
}

/// The cache key.
#[must_use]
pub fn key(
    task_blake3: &str,
    environment_blake3: &str,
    agent_config_blake3: &str,
    seed: u64,
    evaluator: &str,
) -> String {
    jcs_blake3(&serde_json::json!({
        "task_blake3": task_blake3,
        "environment_blake3": environment_blake3,
        "agent_config_blake3": agent_config_blake3,
        "seed": seed,
        "evaluator": evaluator,
    }))
}

impl ResultCache {
    /// A cache in `<state>/result-cache`.
    #[must_use]
    pub fn new(mode: ResultCacheMode, state_root: &Path) -> Self {
        Self {
            mode,
            dir: state_root.join("result-cache"),
        }
    }

    /// The mode.
    #[must_use]
    pub fn mode(&self) -> ResultCacheMode {
        self.mode
    }

    /// A cached record and the path of its trajectory, when the mode reads and the key is present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<(AgentRunRecord, PathBuf)> {
        if self.mode == ResultCacheMode::Off {
            return None;
        }
        let dir = self.dir.join(key);
        let record: AgentRunRecord =
            serde_json::from_slice(&std::fs::read(dir.join("record.json")).ok()?).ok()?;
        Some((record, dir.join("trajectory.jsonl")))
    }

    /// Stores a finished run, when the mode writes. Runs that ended in a harness error are not cached.
    ///
    /// # Errors
    ///
    /// [`BenchError::Io`].
    pub fn put(&self, key: &str, record: &AgentRunRecord, trajectory: &Path) -> BenchResult<()> {
        if self.mode != ResultCacheMode::ReadWrite
            || record.outcome == Some(ogun_lineage::AgentRunOutcome::Error)
        {
            return Ok(());
        }
        let dir = self.dir.join(key);
        std::fs::create_dir_all(&dir).map_err(BenchError::io("create a cache entry"))?;
        if trajectory.exists() {
            std::fs::copy(trajectory, dir.join("trajectory.jsonl"))
                .map_err(BenchError::io("cache a trajectory"))?;
        }
        let text =
            serde_json::to_vec_pretty(record).map_err(|e| BenchError::Spec(e.to_string()))?;
        std::fs::write(dir.join("record.json"), text).map_err(BenchError::io("cache a record"))
    }
}
