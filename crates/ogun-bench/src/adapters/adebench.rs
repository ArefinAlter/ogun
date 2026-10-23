//! ADE-bench (dbt Labs; O-06 section 9.3; unverified).
//!
//! ```text
//! ade-bench/
//!   tasks/<task_id>/task.yaml       prompts[0].prompt (or instruction or description), variants[], difficulty, tags
//!   tasks/<task_id>/tests/*.sql     dbt singular tests: the evaluator (staged at /tests after the agent)
//!   tasks/<task_id>/setup.sh        optional; run in the workspace before the agent, then removed
//!   tasks/<task_id>/solution.sh     the reference solution (never shown, not hashed)
//!   shared/projects/<project_type>/<project_name>/      the dbt project: the workspace
//!   shared/databases/duckdb/<db_name>.duckdb             the database, copied into the workspace
//! ```
//!
//! Only DuckDB variants run here (Snowflake variants need credentials a benchmark run must not have). The default
//! evaluator copies the task's tests into the project and runs `dbt test` on them; exit code 0 passes.

use std::path::Path;

use serde_json::Value;

use crate::BenchResult;
use crate::adapters::{Part, digest_parts, invalid, read_text};
use crate::evaluator::{EvaluatorSpec, FULL_SCORE_PPM, ResultSource};
use crate::task::{Task, TaskEnvironment};

const ADAPTER: &str = "ade_bench";
/// Default agent limit.
pub const DEFAULT_AGENT_TIMEOUT_MS: u64 = 1_800_000;

/// Loads every task under `tasks/` that has a DuckDB variant.
///
/// # Errors
///
/// `bench.task_invalid` for the first malformed task.
pub fn load(root: &Path) -> BenchResult<Vec<Task>> {
    let tasks_dir = root.join("tasks");
    let rd = std::fs::read_dir(&tasks_dir)
        .map_err(|_| invalid(ADAPTER, root.display().to_string(), "tasks/ is missing"))?;
    let mut out = vec![];
    for e in rd.flatten() {
        let yaml = e.path().join("task.yaml");
        if yaml.is_file()
            && let Some(t) = load_task(root, &e.path())?
        {
            out.push(t);
        }
    }
    Ok(out)
}

fn instruction_of(doc: &Value) -> Option<String> {
    doc.get("prompts")
        .and_then(Value::as_array)
        .and_then(|p| {
            p.iter()
                .find(|x| x.get("key").and_then(Value::as_str) == Some("base"))
                .or_else(|| p.first())
        })
        .and_then(|p| p.get("prompt"))
        .and_then(Value::as_str)
        .or_else(|| doc.get("instruction").and_then(Value::as_str))
        .or_else(|| doc.get("description").and_then(Value::as_str))
        .map(str::to_owned)
}

fn plain(s: &str) -> bool {
    !s.is_empty() && !s.contains(['/', '\\']) && !s.starts_with('.')
}

/// Loads one task; `None` when it has no DuckDB variant.
///
/// # Errors
///
/// `bench.task_invalid`.
pub fn load_task(root: &Path, dir: &Path) -> BenchResult<Option<Task>> {
    let dir_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    let yaml_text = read_text(ADAPTER, &dir_name, &dir.join("task.yaml"))?;
    let doc: Value = serde_yaml::from_str(&yaml_text)
        .map_err(|_| invalid(ADAPTER, &dir_name, "task.yaml is not valid YAML"))?;
    let id = doc
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or(&dir_name)
        .to_owned();
    let instruction =
        instruction_of(&doc).ok_or_else(|| invalid(ADAPTER, &id, "task.yaml has no prompt"))?;
    let Some(variant) = doc
        .get("variants")
        .and_then(Value::as_array)
        .and_then(|vs| {
            vs.iter()
                .find(|v| v.get("db_type").and_then(Value::as_str) == Some("duckdb"))
        })
    else {
        return Ok(None);
    };
    let field = |k: &str| variant.get(k).and_then(Value::as_str).filter(|s| plain(s));
    let db_name = field("db_name")
        .ok_or_else(|| invalid(ADAPTER, &id, "the duckdb variant has no db_name"))?;
    let project_type = field("project_type").unwrap_or("dbt");
    let project_name = field("project_name").unwrap_or(db_name);
    let project = root
        .join("shared")
        .join("projects")
        .join(project_type)
        .join(project_name);
    if !project.is_dir() {
        return Err(invalid(
            ADAPTER,
            &id,
            format!("shared/projects/{project_type}/{project_name} is missing"),
        ));
    }
    let db = root
        .join("shared")
        .join("databases")
        .join("duckdb")
        .join(format!("{db_name}.duckdb"));
    if !db.is_file() {
        return Err(invalid(
            ADAPTER,
            &id,
            format!("shared/databases/duckdb/{db_name}.duckdb is missing"),
        ));
    }
    let tests = dir.join("tests");
    if !tests.is_dir() {
        return Err(invalid(ADAPTER, &id, "tests/ is missing"));
    }
    let setup = dir.join("setup.sh");
    let task_blake3 = digest_parts(&[
        ("task.yaml", Part::Text(&yaml_text)),
        ("project", Part::Tree(&project)),
        ("database", Part::Tree(&db)),
        ("tests", Part::Tree(&tests)),
        (
            "setup.sh",
            if setup.is_file() {
                Part::Tree(&setup)
            } else {
                Part::Absent
            },
        ),
    ])?;
    Ok(Some(Task {
        benchmark: "ade_bench".into(),
        id,
        version: doc.get("version").and_then(Value::as_str).map(str::to_owned),
        dir: dir.to_path_buf(),
        instruction,
        environment: TaskEnvironment { workdir: "/workspace".into(), ..TaskEnvironment::default() },
        fixtures: vec![(project, String::new()), (db, String::new())],
        benchmark_files: None,
        eval_files: Some(tests),
        setup_script: setup.is_file().then_some(setup),
        evaluator: EvaluatorSpec {
            name: "ade_bench.dbt_test".into(),
            version: "unversioned".into(),
            command: vec![
                "sh".into(),
                "-c".into(),
                "mkdir -p {workspace}/tests/ogun_eval && cp {tests}/*.sql {workspace}/tests/ogun_eval/ && \
                 cd {workspace} && dbt test --select path:tests/ogun_eval"
                    .into(),
            ],
            timeout_ms: 600_000,
            result: ResultSource::ExitCode,
            pass_threshold_ppm: FULL_SCORE_PPM,
        },
        agent_timeout_ms: DEFAULT_AGENT_TIMEOUT_MS,
        task_blake3,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duckdb_variant_loads_and_snowflake_only_tasks_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::create_dir_all(r.join("shared/projects/dbt/airbnb/models")).unwrap();
        std::fs::create_dir_all(r.join("shared/databases/duckdb")).unwrap();
        std::fs::write(r.join("shared/databases/duckdb/airbnb.duckdb"), b"db").unwrap();
        std::fs::create_dir_all(r.join("tasks/airbnb001/tests")).unwrap();
        std::fs::write(
            r.join("tasks/airbnb001/tests/t.sql"),
            "select 1 where false",
        )
        .unwrap();
        std::fs::write(
            r.join("tasks/airbnb001/task.yaml"),
            "task_id: airbnb001\nprompts:\n  - key: base\n    prompt: Fix the model.\nvariants:\n  - db_type: duckdb\n    db_name: airbnb\n    project_type: dbt\n    project_name: airbnb\n",
        )
        .unwrap();
        std::fs::create_dir_all(r.join("tasks/sf001/tests")).unwrap();
        std::fs::write(
            r.join("tasks/sf001/task.yaml"),
            "task_id: sf001\nprompts:\n  - key: base\n    prompt: x\nvariants:\n  - db_type: snowflake\n    db_name: y\n",
        )
        .unwrap();
        let tasks = load(r).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].instruction, "Fix the model.");
        assert_eq!(tasks[0].fixtures.len(), 2);
    }
}
