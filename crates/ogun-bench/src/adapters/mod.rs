//! Adapters from existing benchmark formats to [`Task`]s (O-06 section 9, ADR 0005 decision 4).
//!
//! Ogun does not define a competing task format. Each adapter reads a benchmark's own directory layout and keeps
//! its official evaluator. The layouts below are what O-06 section 9 describes; they have not been checked against
//! the upstream repositories yet (unverified), so every adapter fails with `bench.task_invalid` naming the missing
//! file rather than guessing.

pub mod adebench;
pub mod harbor;
pub mod spider2;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::evaluator::ResultSource;
use crate::task::Task;
use crate::{BenchError, BenchResult};

/// One benchmark in a matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSource {
    /// `harbor` (Terminal-Bench 2.0 and other Harbor tasks), `spider2_dbt` or `ade_bench`.
    pub adapter: String,
    /// The task directory (Harbor), the `spider2-dbt` directory, or the ADE-bench checkout.
    pub path: PathBuf,
    /// Task ids to run; empty runs all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    /// At most this many tasks (after the id filter), in id order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Replace the adapter's default evaluator command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_command: Option<Vec<String>>,
    /// Replace the adapter's default result source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_result: Option<ResultSource>,
    /// Replace the task's agent time limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_timeout_ms: Option<u64>,
}

/// The adapters this build has.
pub const ADAPTERS: &[&str] = &["harbor", "spider2_dbt", "ade_bench"];

/// Loads the tasks of one benchmark source.
///
/// # Errors
///
/// [`BenchError::Spec`] for an unknown adapter; the adapter's own errors.
pub fn load(source: &BenchmarkSource) -> BenchResult<Vec<Task>> {
    let mut tasks = match source.adapter.as_str() {
        "harbor" => harbor::load(&source.path)?,
        "spider2_dbt" => spider2::load(&source.path)?,
        "ade_bench" => adebench::load(&source.path)?,
        other => {
            return Err(BenchError::Spec(format!(
                "unknown adapter {other}; known: {}",
                ADAPTERS.join(", ")
            )));
        }
    };
    if !source.tasks.is_empty() {
        tasks.retain(|t| source.tasks.contains(&t.id));
        if let Some(missing) = source
            .tasks
            .iter()
            .find(|id| !tasks.iter().any(|t| &&t.id == id))
        {
            return Err(BenchError::Spec(format!(
                "task {missing} is not in {}",
                source.path.display()
            )));
        }
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(n) = source.limit {
        tasks.truncate(n);
    }
    for t in &mut tasks {
        if let Some(cmd) = &source.evaluator_command {
            t.evaluator.command.clone_from(cmd);
            t.evaluator.name = format!("{}.custom", t.benchmark);
        }
        if let Some(r) = &source.evaluator_result {
            t.evaluator.result = r.clone();
        }
        if let Some(ms) = source.agent_timeout_ms {
            t.agent_timeout_ms = ms;
        }
    }
    Ok(tasks)
}

/// BLAKE3 over named parts: text members and directory trees, in the order given.
pub(crate) fn digest_parts(parts: &[(&str, Part<'_>)]) -> BenchResult<String> {
    let mut h = blake3::Hasher::new();
    for (name, part) in parts {
        h.update(name.as_bytes());
        h.update(&[0]);
        match part {
            Part::Text(t) => {
                h.update(t.as_bytes());
            }
            Part::Tree(p) => {
                if p.is_file() {
                    let bytes = std::fs::read(p)
                        .map_err(BenchError::io(format!("read {}", p.display())))?;
                    h.update(blake3::hash(&bytes).as_bytes());
                } else {
                    let snap = crate::workspace::snapshot(p, &[])?;
                    h.update(crate::workspace::tree_digest(&snap).as_bytes());
                }
            }
            Part::Absent => {
                h.update(b"-");
            }
        }
        h.update(&[0]);
    }
    Ok(h.finalize().to_hex().to_string())
}

/// One input of a task digest.
pub(crate) enum Part<'a> {
    /// Text.
    Text(&'a str),
    /// A file or directory.
    Tree(&'a Path),
    /// Nothing (keeps positions stable).
    Absent,
}

pub(crate) fn invalid(
    adapter: &'static str,
    task: impl Into<String>,
    reason: impl Into<String>,
) -> BenchError {
    BenchError::TaskInvalid {
        adapter,
        task: task.into(),
        reason: reason.into(),
    }
}

pub(crate) fn read_text(adapter: &'static str, task: &str, p: &Path) -> BenchResult<String> {
    std::fs::read_to_string(p)
        .map_err(|_| invalid(adapter, task, format!("cannot read {}", p.display())))
}

/// Seconds as a TOML or YAML number (integer or decimal) to milliseconds.
pub(crate) fn secs_to_ms(v: Option<f64>) -> Option<u64> {
    v.filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s * 1000.0).round() as u64)
}

/// `2G`, `512M`, `4096` (MiB) to MiB.
pub(crate) fn memory_mb(text: &str) -> Option<u64> {
    let t = text.trim().to_ascii_uppercase();
    let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let n: u64 = num.parse().ok()?;
    match unit.trim_end_matches('B').trim_end_matches('I') {
        "" | "M" => Some(n),
        "G" => n.checked_mul(1024),
        "K" => Some(n / 1024),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units() {
        assert_eq!(memory_mb("2G"), Some(2048));
        assert_eq!(memory_mb("512M"), Some(512));
        assert_eq!(memory_mb("4096"), Some(4096));
        assert_eq!(memory_mb("1GiB"), Some(1024));
        assert_eq!(secs_to_ms(Some(900.0)), Some(900_000));
        assert_eq!(secs_to_ms(Some(-1.0)), None);
    }
}
