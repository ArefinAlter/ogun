//! The sample suite Ogun Studio ships: four small tasks in the Spider 2.0-DBT directory layout, a scripted
//! reference agent, an agent that does nothing, and an evaluator, so a person can run a whole matrix on the local
//! runtime and see the profile work: a fresh workspace per run, the network denied, a hash-chained trajectory, an
//! evaluator that scores the result and a results table.
//!
//! The agents here are scripts, not models. They prove the harness (isolation, scoring, trajectories, lineage),
//! not any model's ability. The fourth task asks the agent to try the engine's loopback probe address and report
//! whether it got through: it passes only when the environment really denies the network.

use std::io::Write as _;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::adapters::BenchmarkSource;
use crate::agent::{AgentSpec, Placement};
use crate::cache::ResultCacheMode;
use crate::env::{EnvironmentSpec, NetworkPolicy, Runtime};
use crate::matrix::MatrixSpec;
use crate::{BenchError, BenchResult};

/// The name of the benchmark source this suite is read as.
pub const ADAPTER: &str = "spider2_dbt";
/// The variable naming the loopback probe address the fourth task tries.
pub const PROBE_VAR: &str = "PROBE_ADDR";

const CSV: &str = "city,amount\nlagos,120\naccra,75\nnairobi,310\ndakar,45\nkigali,150\n";

struct SampleTask {
    id: &'static str,
    instruction: &'static str,
    gold: &'static str,
}

const TASKS: &[SampleTask] = &[
    SampleTask {
        id: "sum-amounts",
        instruction: "data.csv is in the working directory. Write the total of the amount column to answer.txt.",
        gold: "700",
    },
    SampleTask {
        id: "count-rows",
        instruction: "data.csv is in the working directory. Write the number of data rows (not counting the header) to answer.txt.",
        gold: "5",
    },
    SampleTask {
        id: "largest-city",
        instruction: "data.csv is in the working directory. Write the name of the city with the largest amount to answer.txt.",
        gold: "nairobi",
    },
    SampleTask {
        id: "network-denied",
        instruction: "The environment variable PROBE_ADDR holds a loopback address that something on this computer is listening on. Try to connect to it. Write `blocked` to answer.txt if you cannot, `reachable` if you can.",
        gold: "blocked",
    },
];

/// The task ids of the suite.
#[must_use]
pub fn task_ids() -> Vec<&'static str> {
    TASKS.iter().map(|t| t.id).collect()
}

/// Writes the suite under `root` (replacing what is there).
///
/// # Errors
///
/// [`BenchError::Io`].
pub fn write_suite(root: &Path) -> BenchResult<()> {
    let io = |what: &'static str| BenchError::io(what);
    let _ = std::fs::remove_dir_all(root);
    let examples = root.join("examples");
    let mut list = String::new();
    for t in TASKS {
        let dir = examples.join(t.id);
        std::fs::create_dir_all(&dir).map_err(io("create a task directory"))?;
        std::fs::write(dir.join("data.csv"), CSV).map_err(io("write a task's data"))?;
        let gold = root.join("evaluation_suite").join("gold").join(t.id);
        std::fs::create_dir_all(&gold).map_err(io("create a gold directory"))?;
        std::fs::write(gold.join("answer.txt"), t.gold).map_err(io("write a gold answer"))?;
        list.push_str(&json!({"instance_id": t.id, "instruction": t.instruction}).to_string());
        list.push('\n');
    }
    std::fs::write(examples.join("spider2-dbt.jsonl"), list).map_err(io("write the task list"))?;
    Ok(())
}

/// The matrix of the sample suite: the reference agent and the idle agent over every task, with `exe` (this
/// program) as both agents' and the evaluator's command.
#[must_use]
pub fn matrix(
    root: &Path,
    exe: &Path,
    runtime: Runtime,
    probe_addr: Option<&str>,
    seeds: Vec<u64>,
) -> MatrixSpec {
    let exe = exe.display().to_string();
    let agent = |name: &str, mode: &str| {
        let mut env = std::collections::BTreeMap::new();
        if let Some(p) = probe_addr {
            env.insert(PROBE_VAR.to_owned(), p.to_owned());
        }
        AgentSpec {
            name: name.into(),
            version: "1".into(),
            model: None,
            placement: Placement::Container,
            command: vec![
                exe.clone(),
                "sample-agent".into(),
                mode.into(),
                "--task".into(),
                "{task_id}".into(),
                "--workspace".into(),
                "{workspace}".into(),
                "--trajectory".into(),
                "{trajectory_file}".into(),
            ],
            pass_env: vec![],
            env,
            timeout_ms: Some(60_000),
            prompt_file: None,
            tools_file: None,
        }
    };
    MatrixSpec {
        name: "Ogun sample suite".into(),
        benchmarks: vec![BenchmarkSource {
            adapter: ADAPTER.into(),
            path: root.to_path_buf(),
            tasks: vec![],
            limit: None,
            evaluator_command: Some(vec![
                exe.clone(),
                "sample-eval".into(),
                "--task".into(),
                "{task_id}".into(),
                "--workspace".into(),
                "{workspace}".into(),
                "--tests".into(),
                "{tests}".into(),
            ]),
            evaluator_result: None,
            agent_timeout_ms: Some(60_000),
        }],
        agents: vec![
            agent("reference-script", "solve"),
            agent("idle-script", "idle"),
        ],
        seeds,
        environment: EnvironmentSpec {
            runtime,
            agent_network: NetworkPolicy::Denied,
            evaluator_network: NetworkPolicy::Denied,
            allow_unisolated_local: false,
            ..EnvironmentSpec::default()
        },
        result_cache: ResultCacheMode::Off,
        keep_environments: false,
        lineage_namespace: None,
    }
}

fn line(out: &mut std::fs::File, v: &serde_json::Value) -> std::io::Result<()> {
    writeln!(out, "{v}")
}

/// The reference script: reads `data.csv`, computes the answer for `task` and writes `answer.txt`, reporting
/// what it did to its trajectory file. `idle` does nothing.
///
/// # Errors
///
/// An I/O error reading the data or writing the answer.
pub fn run_agent(
    mode: &str,
    task: &str,
    workspace: &Path,
    trajectory: &Path,
    probe: Option<&str>,
) -> std::io::Result<()> {
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(trajectory)?;
    line(
        &mut log,
        &json!({"type": "message", "role": "assistant", "untrusted_text": format!("starting task {task} as {mode}")}),
    )?;
    if mode == "idle" {
        return Ok(());
    }
    let answer = if task == "network-denied" {
        let reachable = probe
            .and_then(|a| a.parse::<SocketAddr>().ok())
            .is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok());
        line(
            &mut log,
            &json!({"type": "tool.call", "tool": "tcp_connect", "untrusted_args": probe.unwrap_or("no address given")}),
        )?;
        line(
            &mut log,
            &json!({"type": "tool.result", "tool": "tcp_connect", "untrusted_output": if reachable { "connected" } else { "blocked" }}),
        )?;
        if reachable { "reachable" } else { "blocked" }.to_owned()
    } else {
        line(
            &mut log,
            &json!({"type": "tool.shell", "untrusted_command": "read data.csv"}),
        )?;
        let text = std::fs::read_to_string(workspace.join("data.csv"))?;
        let rows: Vec<(String, i64)> = text
            .lines()
            .skip(1)
            .filter_map(|l| {
                let (city, amount) = l.split_once(',')?;
                Some((city.to_owned(), amount.trim().parse().ok()?))
            })
            .collect();
        match task {
            "sum-amounts" => rows.iter().map(|r| r.1).sum::<i64>().to_string(),
            "count-rows" => rows.len().to_string(),
            "largest-city" => rows
                .iter()
                .max_by_key(|r| r.1)
                .map(|r| r.0.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    };
    line(
        &mut log,
        &json!({"type": "tool.call", "tool": "write_file", "untrusted_args": "answer.txt"}),
    )?;
    std::fs::write(workspace.join("answer.txt"), format!("{answer}\n"))?;
    line(
        &mut log,
        &json!({"type": "usage", "input_tokens": 0, "output_tokens": 0, "cost_usd_micro": 0}),
    )?;
    Ok(())
}

/// The evaluator: compares `answer.txt` with the gold answer staged under `tests`. Prints one JSON object.
#[must_use]
pub fn evaluate(task: &str, workspace: &Path, tests: &Path) -> serde_json::Value {
    let read = |p: PathBuf| std::fs::read_to_string(p).ok().map(|s| s.trim().to_owned());
    let given = read(workspace.join("answer.txt"));
    let gold = read(tests.join("gold").join(task).join("answer.txt"));
    let passed = matches!((&given, &gold), (Some(a), Some(b)) if a == b);
    json!({"passed": passed, "score": if passed { 1 } else { 0 }, "task": task})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_script_passes_and_the_idle_script_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("suite");
        write_suite(&root).unwrap();
        let tasks = crate::adapters::load(&BenchmarkSource {
            adapter: ADAPTER.into(),
            path: root.clone(),
            tasks: vec![],
            limit: None,
            evaluator_command: None,
            evaluator_result: None,
            agent_timeout_ms: None,
        })
        .unwrap();
        assert_eq!(tasks.len(), TASKS.len());
        let tests = root.join("evaluation_suite");
        for t in TASKS {
            for (mode, expect) in [("solve", true), ("idle", false)] {
                let ws = dir.path().join(format!("{}-{mode}", t.id));
                std::fs::create_dir_all(&ws).unwrap();
                std::fs::copy(
                    root.join("examples").join(t.id).join("data.csv"),
                    ws.join("data.csv"),
                )
                .unwrap();
                run_agent(mode, t.id, &ws, &ws.join("traj.jsonl"), None).unwrap();
                let v = evaluate(t.id, &ws, &tests);
                assert_eq!(v["passed"], expect, "{} {mode}", t.id);
            }
        }
    }
}
