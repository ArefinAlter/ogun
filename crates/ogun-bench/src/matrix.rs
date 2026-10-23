//! Matrix runs: agents x tasks x seeds (O-06 section 10).
//!
//! Every combination is one agent run in a fresh environment. Runs execute one at a time, in task, agent, seed
//! order; a failure of one run is recorded and the matrix goes on. The results directory holds:
//!
//! ```text
//! <out>/results.json        rows and per-agent summaries (integers only)
//! <out>/results.md          the same as Markdown tables
//! <out>/lineage.jsonl       one OpenLineage event per agent run (O-05 section 6)
//! <out>/matrix.json         the specification that produced them
//! <out>/runs/<agr_id>/      record.json, trajectory.jsonl, evaluator.json, logs/
//! ```

use std::path::Path;

use ogun_lineage::openlineage::agent_run_event;
use ogun_lineage::{
    AgentRunOutcome, AgentRunRecord, CostTotals, LineageConfig, Score, TrajectoryKind,
    TrajectoryWriter, verify_trajectory,
};
use ogun_types::{AgentRunId, IdGenerator, Timestamp};
use serde::{Deserialize, Serialize};

use crate::adapters::{self, BenchmarkSource};
use crate::agent::{self, AgentSpec};
use crate::cache::{self, ResultCache, ResultCacheMode};
use crate::env::{Environment, EnvironmentSpec, Runtime, check_isolated_state};
use crate::evaluator::{self, EvalResult};
use crate::task::Task;
use crate::workspace;
use crate::{BenchError, BenchResult, jcs_blake3};

fn default_seeds() -> Vec<u64> {
    vec![0]
}

/// A matrix specification (`matrix.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixSpec {
    /// Name.
    pub name: String,
    /// Benchmarks and their task filters.
    pub benchmarks: Vec<BenchmarkSource>,
    /// Agents under test.
    pub agents: Vec<AgentSpec>,
    /// Seeds; each task runs once per agent and seed.
    #[serde(default = "default_seeds")]
    pub seeds: Vec<u64>,
    /// How environments are built.
    #[serde(default)]
    pub environment: EnvironmentSpec,
    /// Result cache.
    #[serde(default)]
    pub result_cache: ResultCacheMode,
    /// Keep environment directories after their runs (for debugging).
    #[serde(default)]
    pub keep_environments: bool,
    /// OpenLineage namespace for `lineage.jsonl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_namespace: Option<String>,
}

/// One row of the results table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRow {
    /// The agent run.
    pub agent_run: AgentRunId,
    /// Benchmark family.
    pub benchmark: String,
    /// Task id.
    pub task_id: String,
    /// Agent name.
    pub agent: String,
    /// Agent version.
    pub agent_version: String,
    /// Model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Seed.
    pub seed: u64,
    /// Outcome.
    pub outcome: AgentRunOutcome,
    /// Passed.
    pub passed: bool,
    /// Score in parts per million.
    pub score_ppm: u32,
    /// Agent wall time.
    pub agent_ms: u64,
    /// Run wall time.
    pub wall_ms: u64,
    /// Tokens and cost.
    pub totals: CostTotals,
    /// Tool calls.
    pub tool_calls: u64,
    /// Tool calls with a non-zero exit code.
    pub failed_tool_calls: u64,
    /// Shell commands repeated verbatim.
    pub repeated_commands: u64,
    /// Files created, modified or deleted.
    pub files_touched: u64,
    /// From the result cache.
    pub cached: bool,
    /// `container`, `local_netns` or `unisolated_local`.
    pub isolation: String,
    /// Harness error code, for outcome `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Per-agent summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Agent name.
    pub agent: String,
    /// Agent version.
    pub agent_version: String,
    /// Model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Runs.
    pub runs: u64,
    /// Runs the harness could not complete (not counted in rates).
    pub errors: u64,
    /// Runs scored.
    pub scored: u64,
    /// Passed.
    pub passed: u64,
    /// Passed per scored run, parts per million.
    pub pass_rate_ppm: u32,
    /// Mean score over scored runs, parts per million.
    pub mean_score_ppm: u32,
    /// Agent timeouts.
    pub timed_out: u64,
    /// Tokens and cost over all runs.
    pub totals: CostTotals,
    /// Cost per passed run (VISION section 12: C_s), when any passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_success_usd_micro: Option<u64>,
    /// Mean agent wall time.
    pub mean_agent_ms: u64,
    /// Tool calls.
    pub tool_calls: u64,
    /// Useful tool calls (not failed, not repeated) per tool call, parts per million.
    pub tool_efficiency_ppm: u32,
}

/// What a matrix produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixResults {
    /// Matrix name.
    pub name: String,
    /// BLAKE3 of the specification.
    pub matrix_blake3: String,
    /// Start.
    pub started_at: Timestamp,
    /// End.
    pub finished_at: Timestamp,
    /// One row per run.
    pub rows: Vec<RunRow>,
    /// One summary per agent configuration.
    pub agents: Vec<AgentSummary>,
}

fn ratio_ppm(num: u64, den: u64) -> u32 {
    if den == 0 {
        return 0;
    }
    u32::try_from((u128::from(num) * 1_000_000 / u128::from(den)).min(1_000_000))
        .unwrap_or(1_000_000)
}

/// Summarises rows per agent configuration, in first-seen order.
#[must_use]
pub fn summarise(rows: &[RunRow]) -> Vec<AgentSummary> {
    let mut keys: Vec<(String, String, Option<String>)> = vec![];
    for r in rows {
        let k = (r.agent.clone(), r.agent_version.clone(), r.model.clone());
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys.into_iter()
        .map(|(agent, agent_version, model)| {
            let mine: Vec<&RunRow> = rows
                .iter()
                .filter(|r| {
                    r.agent == agent && r.agent_version == agent_version && r.model == model
                })
                .collect();
            let runs = mine.len() as u64;
            let errors = mine
                .iter()
                .filter(|r| r.outcome == AgentRunOutcome::Error)
                .count() as u64;
            let scored: Vec<&&RunRow> = mine
                .iter()
                .filter(|r| r.outcome != AgentRunOutcome::Error)
                .collect();
            let passed = scored.iter().filter(|r| r.passed).count() as u64;
            let score_sum: u64 = scored.iter().map(|r| u64::from(r.score_ppm)).sum();
            let mut totals = CostTotals::default();
            for r in &mine {
                totals.input_tokens += r.totals.input_tokens;
                totals.output_tokens += r.totals.output_tokens;
                totals.cost_usd_micro += r.totals.cost_usd_micro;
                totals.bytes += r.totals.bytes;
                totals.model_requests += r.totals.model_requests;
            }
            let tool_calls: u64 = mine.iter().map(|r| r.tool_calls).sum();
            let wasted: u64 = mine
                .iter()
                .map(|r| r.failed_tool_calls + r.repeated_commands)
                .sum();
            let n_scored = scored.len() as u64;
            AgentSummary {
                agent,
                agent_version,
                model,
                runs,
                errors,
                scored: n_scored,
                passed,
                pass_rate_ppm: ratio_ppm(passed, n_scored),
                mean_score_ppm: score_sum
                    .checked_div(n_scored)
                    .and_then(|m| u32::try_from(m).ok())
                    .unwrap_or(0),
                timed_out: mine
                    .iter()
                    .filter(|r| r.outcome == AgentRunOutcome::TimedOut)
                    .count() as u64,
                cost_per_success_usd_micro: (passed > 0).then(|| totals.cost_usd_micro / passed),
                totals,
                mean_agent_ms: mine
                    .iter()
                    .map(|r| r.agent_ms)
                    .sum::<u64>()
                    .checked_div(runs)
                    .unwrap_or(0),
                tool_calls,
                tool_efficiency_ppm: if tool_calls == 0 {
                    1_000_000
                } else {
                    ratio_ppm(tool_calls.saturating_sub(wasted), tool_calls)
                },
            }
        })
        .collect()
}

fn pct(ppm: u32) -> String {
    format!("{}.{}%", ppm / 10_000, (ppm % 10_000) / 1_000)
}

fn usd(micro: u64) -> String {
    format!("${}.{:04}", micro / 1_000_000, (micro % 1_000_000) / 100)
}

fn secs(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn outcome_name(o: AgentRunOutcome) -> &'static str {
    match o {
        AgentRunOutcome::Completed => "completed",
        AgentRunOutcome::Failed => "failed",
        AgentRunOutcome::TimedOut => "timed_out",
        AgentRunOutcome::Cancelled => "cancelled",
        AgentRunOutcome::Error => "error",
    }
}

/// The results as Markdown.
#[must_use]
pub fn render_markdown(r: &MatrixResults) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Benchmark results: {}\n\n", cell(&r.name)));
    s.push_str(&format!(
        "{} runs from {} to {}. Matrix BLAKE3 `{}`. The JSON file holds the same numbers as integers \
         (`score_ppm`, `cost_usd_micro`, `_ms`); rates here are rounded down.\n\n",
        r.rows.len(),
        r.started_at.to_rfc3339(),
        r.finished_at.to_rfc3339(),
        r.matrix_blake3
    ));
    s.push_str("## Agents\n\n");
    s.push_str("| Agent | Model | Runs | Errors | Passed | Pass rate | Mean score | Timeouts | Cost | Cost per success | Tokens in | Tokens out | Mean agent time | Tool efficiency |\n");
    s.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for a in &r.agents {
        s.push_str(&format!(
            "| {}@{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&a.agent),
            cell(&a.agent_version),
            cell(a.model.as_deref().unwrap_or("-")),
            a.runs,
            a.errors,
            a.passed,
            pct(a.pass_rate_ppm),
            pct(a.mean_score_ppm),
            a.timed_out,
            usd(a.totals.cost_usd_micro),
            a.cost_per_success_usd_micro
                .map_or_else(|| "-".to_owned(), usd),
            a.totals.input_tokens,
            a.totals.output_tokens,
            secs(a.mean_agent_ms),
            pct(a.tool_efficiency_ppm),
        ));
    }
    s.push_str("\n## Runs\n\n");
    s.push_str("| Benchmark | Task | Agent | Seed | Outcome | Passed | Score | Agent time | Cost | Tool calls | Files touched | Isolation | Cached | Agent run |\n");
    s.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for row in &r.rows {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {}{} | {} | {} | {} | {} | {} | {} | {} | {} | `{}` |\n",
            cell(&row.benchmark),
            cell(&row.task_id),
            cell(&row.agent),
            row.seed,
            outcome_name(row.outcome),
            row.error_code
                .as_deref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default(),
            if row.passed { "yes" } else { "no" },
            pct(row.score_ppm),
            secs(row.agent_ms),
            usd(row.totals.cost_usd_micro),
            row.tool_calls,
            row.files_touched,
            row.isolation,
            if row.cached { "yes" } else { "no" },
            row.agent_run,
        ));
    }
    s
}

fn row_of(record: &AgentRunRecord, isolation: &str, failed_tools: u64, repeated: u64) -> RunRow {
    RunRow {
        agent_run: record.id,
        benchmark: record.task.benchmark.clone(),
        task_id: record.task.task_id.clone(),
        agent: record.agent.name.clone(),
        agent_version: record.agent.version.clone(),
        model: record.agent.model.clone(),
        seed: record.seed.unwrap_or(0),
        outcome: record.outcome.unwrap_or(AgentRunOutcome::Error),
        passed: record.score.as_ref().is_some_and(|s| s.passed),
        score_ppm: record.score.as_ref().map_or(0, |s| s.score_ppm),
        agent_ms: record.agent_ms,
        wall_ms: record.wall_ms,
        totals: record.totals,
        tool_calls: record.tool_calls,
        failed_tool_calls: failed_tools,
        repeated_commands: repeated,
        files_touched: record.files_created + record.files_modified + record.files_deleted,
        cached: record.cached,
        isolation: isolation.to_owned(),
        error_code: record.error_code.clone(),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> BenchResult<()> {
    let text = serde_json::to_vec_pretty(value).map_err(|e| BenchError::Spec(e.to_string()))?;
    std::fs::write(path, text).map_err(BenchError::io(format!("write {}", path.display())))
}

fn elapsed_ms(from: Timestamp) -> u64 {
    Timestamp::now()
        .unix_millis()
        .saturating_sub(from.unix_millis())
        .max(0)
        .unsigned_abs()
}

struct Ctx<'a> {
    spec: &'a MatrixSpec,
    state_root: &'a Path,
    out_dir: &'a Path,
    ids: IdGenerator,
    cache: ResultCache,
}

fn db_states(w: &mut TrajectoryWriter, snap: &workspace::Snapshot, phase: &str) -> BenchResult<()> {
    for (path, e) in snap.iter().filter(|(p, _)| p.ends_with(".duckdb")) {
        w.append(
            Timestamp::now(),
            TrajectoryKind::DbState {
                path: path.clone(),
                blake3: e.blake3.clone(),
                phase: phase.to_owned(),
            },
        )?;
    }
    Ok(())
}

async fn run_one(cx: &Ctx<'_>, task: &Task, agent_spec: &AgentSpec, seed: u64) -> RunRow {
    let id = cx.ids.agent_run_id();
    let started = Timestamp::now();
    let run_dir = cx.out_dir.join("runs").join(id.to_string());
    let mut record = AgentRunRecord {
        id,
        agent: agent_spec.info(),
        task: task.task_ref(),
        environment: None,
        environment_blake3: None,
        evaluator: Some(cx.ids.evaluator_id()),
        evaluator_version: Some(task.evaluator.label()),
        seed: Some(seed),
        run_id: None,
        started_at: started,
        finished_at: None,
        outcome: None,
        exit_code: None,
        agent_ms: 0,
        wall_ms: 0,
        totals: CostTotals::default(),
        tool_calls: 0,
        files_created: 0,
        files_modified: 0,
        files_deleted: 0,
        trajectory: None,
        score: None,
        cached: false,
        error_code: None,
    };
    let fail = |mut record: AgentRunRecord, e: &BenchError, isolation: &str| {
        tracing::warn!(task = %task.id, agent = %agent_spec.name, code = e.code(), error = %e, "run failed in the harness");
        record.outcome = Some(AgentRunOutcome::Error);
        record.error_code = Some(e.code().to_owned());
        record.finished_at = Some(Timestamp::now());
        record.wall_ms = elapsed_ms(started);
        let _ = std::fs::create_dir_all(&run_dir);
        let _ = write_json(&run_dir.join("record.json"), &record);
        row_of(&record, isolation, 0, 0)
    };
    if cx.spec.environment.runtime == Runtime::Local
        && (task.environment.build_context.is_some() || task.environment.image.is_some())
    {
        let e = BenchError::TaskUnsupported {
            task: task.id.clone(),
            reason: "this task ships a container image; use the docker or podman runtime".into(),
        };
        return fail(record, &e, "none");
    }
    if let Err(e) = std::fs::create_dir_all(&run_dir) {
        return fail(
            record,
            &BenchError::io("create the run directory")(e),
            "none",
        );
    }
    let env = match Environment::create(&cx.spec.environment, task, cx.state_root, &cx.ids).await {
        Ok(env) => env,
        Err(e) => return fail(record, &e, "none"),
    };
    record.environment = Some(env.id);
    record.environment_blake3 = Some(env.digest.clone());
    let isolation = env.isolation;

    let key = cache::key(
        &task.task_blake3,
        &env.digest,
        &record.agent.config_blake3,
        seed,
        &task.evaluator.label(),
    );
    if let Some((mut cached, trajectory)) = cx.cache.get(&key) {
        env.teardown().await;
        env.remove_dir();
        if trajectory.exists() {
            let _ = std::fs::copy(&trajectory, run_dir.join("trajectory.jsonl"));
        }
        cached.cached = true;
        let _ = write_json(&run_dir.join("record.json"), &cached);
        tracing::info!(task = %task.id, agent = %agent_spec.name, seed, "result cache hit");
        return row_of(&cached, isolation, 0, 0);
    }

    let result = drive(cx, task, agent_spec, seed, &env, &mut record, &run_dir).await;
    let _ = workspace::copy_dir(&env.logs, &run_dir.join("logs"));
    env.teardown().await;
    if !cx.spec.keep_environments {
        env.remove_dir();
    }
    let (failed_tools, repeated) = match result {
        Ok(v) => v,
        Err(e) => return fail(record, &e, isolation),
    };
    record.finished_at = Some(Timestamp::now());
    record.wall_ms = elapsed_ms(started);
    let _ = write_json(&run_dir.join("record.json"), &record);
    if let Err(e) = cx
        .cache
        .put(&key, &record, &run_dir.join("trajectory.jsonl"))
    {
        tracing::warn!(code = e.code(), "could not store a result in the cache");
    }
    row_of(&record, isolation, failed_tools, repeated)
}

/// Runs the agent and the evaluator, filling `record`. Returns failed and repeated tool-call counts.
async fn drive(
    _cx: &Ctx<'_>,
    task: &Task,
    agent_spec: &AgentSpec,
    seed: u64,
    env: &Environment,
    record: &mut AgentRunRecord,
    run_dir: &Path,
) -> BenchResult<(u64, u64)> {
    let traj_path = run_dir.join("trajectory.jsonl");
    let mut w = TrajectoryWriter::create(&traj_path, record.id, record.started_at)?;
    for s in agent_spec.secret_values() {
        w.add_secret(s);
    }
    w.append(
        Timestamp::now(),
        TrajectoryKind::TaskStarted {
            task_id: task.id.clone(),
            task_blake3: task.task_blake3.clone(),
            environment_blake3: env.digest.clone(),
            seed: Some(seed),
        },
    )?;
    env.run_setup(task).await?;
    let before = workspace::snapshot(&env.workspace, &[])?;
    w.append(
        Timestamp::now(),
        TrajectoryKind::WorkspaceSnapshot {
            tree_blake3: workspace::tree_digest(&before),
            files: before.len() as u64,
        },
    )?;
    db_states(&mut w, &before, "before")?;

    let process = agent::run_agent(agent_spec, task, env, seed, &mut w).await?;
    record.exit_code = process.exit_code;
    record.agent_ms = process.agent_ms;
    agent::ingest_agent_log(&mut w, env)?;

    let after = workspace::snapshot(&env.workspace, &[])?;
    for change in workspace::diff(&before, &after) {
        match &change {
            TrajectoryKind::FileCreated { .. } => record.files_created += 1,
            TrajectoryKind::FilePatch { .. } => record.files_modified += 1,
            TrajectoryKind::FileDeleted { .. } => record.files_deleted += 1,
            _ => {}
        }
        w.append(Timestamp::now(), change)?;
    }
    db_states(&mut w, &after, "after")?;

    env.stage_eval_files(task).await?;
    env.open_evaluator_network().await?;
    w.append(
        Timestamp::now(),
        TrajectoryKind::EvaluatorStarted {
            evaluator: task.evaluator.label(),
        },
    )?;
    let eval: EvalResult = evaluator::evaluate(&task.evaluator, env, &task.id).await?;
    w.append(
        Timestamp::now(),
        TrajectoryKind::EvaluatorFinished {
            passed: eval.passed,
            score_ppm: eval.score_ppm,
            exit_code: eval.exit_code,
            duration_ms: eval.duration_ms,
        },
    )?;
    write_json(&run_dir.join("evaluator.json"), &eval)?;
    let outcome = if process.timed_out {
        AgentRunOutcome::TimedOut
    } else {
        AgentRunOutcome::Completed
    };
    w.append(Timestamp::now(), TrajectoryKind::TaskFinished { outcome })?;
    let tref = w.finish("trajectory.jsonl")?;
    let text = std::fs::read(&traj_path).map_err(BenchError::io("read the trajectory back"))?;
    let summary = verify_trajectory(text.as_slice())?;
    record.totals = summary.totals;
    record.tool_calls = summary.tool_calls;
    record.trajectory = Some(tref);
    record.outcome = Some(outcome);
    record.score = Some(Score {
        passed: eval.passed,
        score_ppm: eval.score_ppm,
        result_blake3: Some(jcs_blake3(&eval.raw)),
    });
    Ok((summary.failed_tool_calls, summary.repeated_commands))
}

/// Runs a matrix and writes its results directory.
///
/// # Errors
///
/// [`BenchError::Spec`] for an invalid specification, [`BenchError::StoreOverlap`] when the state or results
/// directory overlaps the user's Ogun or Zeus state, adapter errors while loading tasks, and I/O errors writing
/// the results. Failures of single runs are rows with outcome `error`, not errors of the matrix.
pub async fn run_matrix(
    spec: &MatrixSpec,
    state_root: &Path,
    out_dir: &Path,
) -> BenchResult<MatrixResults> {
    if spec.agents.is_empty() || spec.benchmarks.is_empty() || spec.seeds.is_empty() {
        return Err(BenchError::Spec(
            "a matrix needs at least one benchmark, agent and seed".into(),
        ));
    }
    for a in &spec.agents {
        a.validate()?;
    }
    check_isolated_state(state_root)?;
    check_isolated_state(out_dir)?;
    std::fs::create_dir_all(out_dir.join("runs"))
        .map_err(BenchError::io("create the results directory"))?;
    std::fs::create_dir_all(state_root).map_err(BenchError::io("create the state directory"))?;
    write_json(&out_dir.join("matrix.json"), spec)?;
    let mut tasks = vec![];
    for b in &spec.benchmarks {
        tasks.extend(adapters::load(b)?);
    }
    let cx = Ctx {
        spec,
        state_root,
        out_dir,
        ids: IdGenerator::new(),
        cache: ResultCache::new(spec.result_cache, state_root),
    };
    let started_at = Timestamp::now();
    let matrix_blake3 = jcs_blake3(spec);
    let mut results = MatrixResults {
        name: spec.name.clone(),
        matrix_blake3,
        started_at,
        finished_at: started_at,
        rows: vec![],
        agents: vec![],
    };
    let total = tasks.len() * spec.agents.len() * spec.seeds.len();
    for task in &tasks {
        for agent in &spec.agents {
            for &seed in &spec.seeds {
                tracing::info!(n = results.rows.len() + 1, total, task = %task.id, agent = %agent.name, seed, "run");
                let row = run_one(&cx, task, agent, seed).await;
                results.rows.push(row);
                results.finished_at = Timestamp::now();
                results.agents = summarise(&results.rows);
                // Partial results survive an interrupted matrix.
                write_json(&out_dir.join("results.json"), &results)?;
            }
        }
    }
    results.finished_at = Timestamp::now();
    results.agents = summarise(&results.rows);
    write_json(&out_dir.join("results.json"), &results)?;
    std::fs::write(out_dir.join("results.md"), render_markdown(&results))
        .map_err(BenchError::io("write results.md"))?;
    let cfg = LineageConfig {
        namespace: spec
            .lineage_namespace
            .clone()
            .unwrap_or_else(|| "ogun://bench".into()),
        ..LineageConfig::default()
    };
    let mut events = vec![];
    for row in &results.rows {
        let path = out_dir
            .join("runs")
            .join(row.agent_run.to_string())
            .join("record.json");
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(rec) = serde_json::from_slice::<AgentRunRecord>(&bytes)
        {
            events.push(agent_run_event(&rec, &cfg));
        }
    }
    let mut f = std::fs::File::create(out_dir.join("lineage.jsonl"))
        .map_err(BenchError::io("write lineage.jsonl"))?;
    ogun_lineage::write_jsonl(&mut f, &events)?;
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Placement;
    use crate::env::NetworkPolicy;
    use std::collections::BTreeMap;

    /// A whole matrix on the local runtime: one Harbor-like task scored by a command evaluator through the
    /// `spider2_dbt`-style JSON-lines adapter is too heavy for a unit test, so this builds a Spider 2.0-DBT layout
    /// with a trivial evaluator override.
    #[tokio::test]
    async fn a_local_matrix_runs_records_and_summarises() {
        let dir = tempfile::tempdir().unwrap();
        let bench = dir.path().join("spider2-dbt");
        std::fs::create_dir_all(bench.join("examples").join("t1")).unwrap();
        std::fs::write(bench.join("examples").join("t1").join("input.txt"), "x").unwrap();
        std::fs::create_dir_all(bench.join("evaluation_suite")).unwrap();
        std::fs::write(
            bench.join("examples").join("spider2-dbt.jsonl"),
            "{\"instance_id\":\"t1\",\"instruction\":\"Write done.txt\"}\n",
        )
        .unwrap();
        let spec = MatrixSpec {
            name: "local smoke".into(),
            benchmarks: vec![BenchmarkSource {
                adapter: "spider2_dbt".into(),
                path: bench,
                tasks: vec![],
                limit: None,
                evaluator_command: Some(vec![
                    "sh".into(),
                    "-c".into(),
                    "test -f {workspace}/done.txt && echo '{\"passed\": true, \"score\": 1}' || echo '{\"passed\": false}'".into(),
                ]),
                evaluator_result: None,
                agent_timeout_ms: Some(10_000),
            }],
            agents: vec![AgentSpec {
                name: "echo-agent".into(),
                version: "1".into(),
                model: None,
                placement: Placement::Container,
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "echo done > done.txt; echo '{\"type\":\"usage\",\"input_tokens\":3,\"output_tokens\":4,\"cost_usd_micro\":5}' > {trajectory_file}".into(),
                ],
                pass_env: vec![],
                env: BTreeMap::new(),
                timeout_ms: None,
                prompt_file: None,
                tools_file: None,
            }],
            seeds: vec![1],
            environment: EnvironmentSpec {
                runtime: Runtime::Local,
                agent_network: NetworkPolicy::Denied,
                evaluator_network: NetworkPolicy::Denied,
                allow_unisolated_local: true,
                ..EnvironmentSpec::default()
            },
            result_cache: ResultCacheMode::Off,
            keep_environments: false,
            lineage_namespace: None,
        };
        let state = dir.path().join("state");
        let out = dir.path().join("out");
        let results = run_matrix(&spec, &state, &out).await.unwrap();
        assert_eq!(results.rows.len(), 1);
        let row = &results.rows[0];
        assert_eq!(row.outcome, AgentRunOutcome::Completed, "{row:?}");
        assert!(row.passed);
        assert_eq!(row.totals.cost_usd_micro, 5);
        assert_eq!(row.files_touched, 1);
        assert_eq!(results.agents[0].pass_rate_ppm, 1_000_000);
        assert!(out.join("results.md").is_file());
        let traj = std::fs::read(
            out.join("runs")
                .join(row.agent_run.to_string())
                .join("trajectory.jsonl"),
        )
        .unwrap();
        verify_trajectory(traj.as_slice()).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("lineage.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}
