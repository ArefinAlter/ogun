//! Spider 2.0-DBT (O-06 section 9.2; unverified).
//!
//! ```text
//! spider2-dbt/
//!   examples/spider2-dbt.jsonl            one {"instance_id", "instruction"} per line
//!   examples/<instance_id>/               the dbt project with its DuckDB file: the workspace
//!   evaluation_suite/                     evaluate.py and gold/<instance_id>/ (staged at /tests after the agent)
//! ```
//!
//! The tasks name no image: the matrix's `environment.image` must provide dbt and DuckDB (or the local runtime
//! must have them). The default evaluator runs the suite's `evaluate.py` on this one result and reads the last
//! JSON object or number it prints; a matrix can replace both with `evaluator_command` and `evaluator_result`.

use std::path::Path;

use serde_json::Value;

use crate::BenchResult;
use crate::adapters::{Part, digest_parts, invalid, read_text};
use crate::evaluator::{EvaluatorSpec, FULL_SCORE_PPM, ResultSource};
use crate::task::{Task, TaskEnvironment};

const ADAPTER: &str = "spider2_dbt";
/// Default agent limit.
pub const DEFAULT_AGENT_TIMEOUT_MS: u64 = 1_800_000;

/// Loads every instance listed in `examples/spider2-dbt.jsonl`.
///
/// # Errors
///
/// `bench.task_invalid` when the list, an instance directory or the evaluation suite is missing.
pub fn load(root: &Path) -> BenchResult<Vec<Task>> {
    let list = root.join("examples").join("spider2-dbt.jsonl");
    let text = read_text(ADAPTER, "spider2-dbt", &list)?;
    let suite = root.join("evaluation_suite");
    if !suite.is_dir() {
        return Err(invalid(
            ADAPTER,
            "spider2-dbt",
            "evaluation_suite/ is missing",
        ));
    }
    let mut out = vec![];
    for (i, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let v: Value = serde_json::from_str(line)
            .map_err(|_| invalid(ADAPTER, format!("line {}", i + 1), "not a JSON object"))?;
        let id = v
            .get("instance_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(ADAPTER, format!("line {}", i + 1), "no instance_id"))?
            .to_owned();
        if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') {
            return Err(invalid(ADAPTER, id, "instance_id is not a plain name"));
        }
        let instruction = v
            .get("instruction")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(ADAPTER, &id, "no instruction"))?
            .to_owned();
        let project = root.join("examples").join(&id);
        if !project.is_dir() {
            return Err(invalid(ADAPTER, &id, "examples/<instance_id>/ is missing"));
        }
        let gold = suite.join("gold").join(&id);
        let task_blake3 = digest_parts(&[
            ("instruction", Part::Text(&instruction)),
            ("project", Part::Tree(&project)),
            (
                "gold",
                if gold.exists() {
                    Part::Tree(&gold)
                } else {
                    Part::Absent
                },
            ),
        ])?;
        out.push(Task {
            benchmark: "spider2_dbt".into(),
            id: id.clone(),
            version: None,
            dir: project.clone(),
            instruction,
            environment: TaskEnvironment {
                workdir: "/workspace".into(),
                ..TaskEnvironment::default()
            },
            fixtures: vec![(project, String::new())],
            benchmark_files: None,
            eval_files: Some(suite.clone()),
            setup_script: None,
            evaluator: EvaluatorSpec {
                name: "spider2_dbt.evaluate".into(),
                version: "unversioned".into(),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "mkdir -p {logs}/verifier/results && rm -rf {logs}/verifier/results/{task_id} && \
                     cp -r {workspace} {logs}/verifier/results/{task_id} && cd {tests} && \
                     python3 evaluate.py --mode dbt --result_dir {logs}/verifier/results"
                        .into(),
                ],
                timeout_ms: 600_000,
                result: ResultSource::Stdout,
                pass_threshold_ppm: FULL_SCORE_PPM,
            },
            agent_timeout_ms: DEFAULT_AGENT_TIMEOUT_MS,
            task_blake3,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_load_with_their_project_as_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::create_dir_all(r.join("examples").join("activity001").join("models")).unwrap();
        std::fs::write(
            r.join("examples")
                .join("activity001")
                .join("dbt_project.yml"),
            "name: activity",
        )
        .unwrap();
        std::fs::create_dir_all(r.join("evaluation_suite").join("gold").join("activity001"))
            .unwrap();
        std::fs::write(
            r.join("examples").join("spider2-dbt.jsonl"),
            "{\"instance_id\": \"activity001\", \"instruction\": \"Build the activity models.\"}\n",
        )
        .unwrap();
        let tasks = load(r).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].fixtures.len(), 1);
        assert!(
            tasks[0]
                .eval_files
                .as_ref()
                .unwrap()
                .ends_with("evaluation_suite")
        );
    }
}
