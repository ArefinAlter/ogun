//! Benchmark runs started from the app (O-06, ADR 0005): `bench.list`, `bench.run`, `bench.status`,
//! `bench.cancel` and `bench.replay`.
//!
//! The daemon never runs a benchmark itself. It starts the `ogun-bench` program that ships beside it, with a
//! cleared environment, a state directory outside Ogun's own (the harness refuses one inside), and a loopback
//! listener the sample suite's network task tries to reach. Everything a run produces (matrix, results,
//! trajectories, lineage) stays in that directory; a run never reads the run store, the artifact store, secrets or
//! `zeusd`, and the network is denied unless a person chose a runtime that cannot deny it (then the run is refused).
//!
//! What runs today: the sample suite (four small tasks in the Spider 2.0-DBT layout, a scripted reference agent and
//! an idle one) on the local runtime (a network namespace on Linux, an AppContainer on Windows) or, where the
//! `docker` or `podman` CLI answers, in containers. The Harbor, Spider 2.0-DBT and ADE-bench adapters exist in
//! `ogun-bench` but are not offered here: they need the benchmarks' own task checkouts and a container runtime, and
//! their layouts are unverified (O-06 section 9).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ogun_api::{ApiError, ApiResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::sync::watch;

use super::{args, out};
use crate::server::Conn;
use crate::state::{Daemon, lock};

const SUITE: &str = "sample";
const MAX_SEEDS: usize = 5;
const MAX_RUNS_LISTED: usize = 50;

/// What is stored about a run, next to its results.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    id: String,
    suite: String,
    runtime: String,
    seeds: Vec<u64>,
    /// `running`, `finished`, `failed`, `cancelled` or `interrupted`.
    state: String,
    started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
    /// Connections the loopback probe accepted while the run was going (the network task's evidence).
    #[serde(default)]
    probe_hits: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The benchmark runs of this daemon.
#[derive(Debug)]
pub struct Benches {
    root: PathBuf,
    home: PathBuf,
    live: Mutex<HashMap<String, watch::Sender<bool>>>,
    doctor: Mutex<Option<(Instant, Value)>>,
}

fn short_hash(p: &Path) -> String {
    blake3::hash(p.to_string_lossy().as_bytes()).to_hex()[..10].to_owned()
}

/// Where this daemon's benchmark state lives: beside Ogun's state, never inside it (the harness refuses that).
fn bench_root(state_dir: &Path) -> PathBuf {
    let key = format!("p-{}", short_hash(state_dir));
    if let Some(p) = std::env::var_os("OGUN_BENCH_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(p).join(key);
    }
    match directories::BaseDirs::new() {
        Some(base) if cfg!(windows) || cfg!(target_os = "macos") => {
            base.data_local_dir().join("OgunBench").join(key)
        }
        Some(base) => base
            .state_dir()
            .unwrap_or_else(|| base.data_local_dir())
            .join("ogun-bench")
            .join(key),
        None => std::env::temp_dir().join("ogun-bench").join(key),
    }
}

impl Benches {
    /// The runs of the daemon whose state is `state_dir`. Runs that were going when it last stopped are marked
    /// `interrupted`.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        let b = Self {
            root: bench_root(state_dir),
            home: state_dir.to_path_buf(),
            live: Mutex::new(HashMap::new()),
            doctor: Mutex::new(None),
        };
        for (dir, mut meta) in b.metas() {
            if meta.state == "running" {
                meta.state = "interrupted".into();
                meta.finished_at = Some(ogun_types::Timestamp::now().to_rfc3339());
                let _ = write_meta(&dir, &meta);
            }
        }
        b
    }

    fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    fn metas(&self) -> Vec<(PathBuf, Meta)> {
        let Ok(rd) = std::fs::read_dir(self.runs_dir()) else {
            return vec![];
        };
        let mut v: Vec<(PathBuf, Meta)> = rd
            .flatten()
            .filter_map(|e| {
                let dir = e.path();
                let meta: Meta =
                    serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
                Some((dir, meta))
            })
            .collect();
        v.sort_by(|a, b| b.1.id.cmp(&a.1.id));
        v
    }

    fn find(&self, id: &str) -> ApiResult<(PathBuf, Meta)> {
        if !id.starts_with("bmr_") || id.contains(['/', '\\', '.']) {
            return Err(ApiError::new(
                "api.invalid_params",
                "not a benchmark run id",
            ));
        }
        let dir = self.runs_dir().join(id);
        let meta: Meta = std::fs::read(dir.join("meta.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or_else(|| ApiError::new("api.invalid_params", "no such benchmark run"))?;
        Ok((dir, meta))
    }
}

fn write_meta(dir: &Path, meta: &Meta) -> ApiResult<()> {
    let bytes = serde_json::to_vec_pretty(meta).map_err(|e| ApiError::internal(e.to_string()))?;
    std::fs::write(dir.join("meta.json"), bytes)
        .map_err(|e| ApiError::unavailable(format!("cannot write the run record ({:?})", e.kind())))
}

/// `ogun-bench`, beside `ogund` (or on the daemon's path in development).
fn bench_exe() -> ApiResult<PathBuf> {
    ogun_ops::tools::find("ogun-bench", &[]).ok_or_else(|| {
        ApiError::new(
            "bench.environment_failed",
            "ogun-bench is not installed next to the engine",
        )
    })
}

/// The few variables the harness itself needs. The agents it runs get an even smaller set of their own.
fn harness_env(cmd: &mut Command, home: &Path, root: &Path) {
    cmd.env_clear();
    for k in ["SystemRoot", "windir", "PATH", "LANG", "TMPDIR"] {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    // The harness reads these only to refuse a state directory that overlaps Ogun's own.
    cmd.env("OGUN_HOME", home);
    cmd.env("OGUN_BENCH_STATE_DIR", root);
}

async fn doctor(d: &Daemon) -> Value {
    {
        let g = lock(&d.studio.bench.doctor);
        if let Some((at, v)) = g.as_ref()
            && at.elapsed() < Duration::from_secs(60)
        {
            return v.clone();
        }
    }
    let value = match bench_exe() {
        Ok(exe) => {
            let mut cmd = Command::new(exe);
            harness_env(&mut cmd, &d.studio.bench.home, &d.studio.bench.root);
            cmd.arg("doctor")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            match tokio::time::timeout(Duration::from_secs(90), cmd.output()).await {
                Ok(Ok(o)) if o.status.success() => {
                    serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null)
                }
                _ => Value::Null,
            }
        }
        Err(_) => Value::Null,
    };
    *lock(&d.studio.bench.doctor) = Some((Instant::now(), value.clone()));
    value
}

fn runtimes(doctor: &Value) -> Value {
    let iso = doctor.get("local_isolation").and_then(Value::as_str);
    let flag = |k: &str| doctor.get(k).and_then(Value::as_bool).unwrap_or(false);
    json!([
        {
            "id": "local",
            "title": "This computer, network denied",
            "available": iso.is_some(),
            "isolation": iso,
            "note": match iso {
                Some("local_appcontainer") => "Commands run in a Windows AppContainer with no network and write access only to the run's workspace and logs.",
                Some("local_netns") => "Commands run in a user and network namespace with no network.",
                _ => "This computer cannot deny the network to a local run (no network namespace, no Windows sandbox helper). Install Docker or Podman to run benchmarks in a container.",
            },
        },
        {
            "id": "docker",
            "title": "Docker container",
            "available": flag("docker"),
            "isolation": if flag("docker") { json!("container") } else { Value::Null },
            "note": if flag("docker") { "Containers with every capability dropped and no network." } else { "The docker command did not answer." },
        },
        {
            "id": "podman",
            "title": "Podman container",
            "available": flag("podman"),
            "isolation": if flag("podman") { json!("container") } else { Value::Null },
            "note": if flag("podman") { "Containers with every capability dropped and no network." } else { "The podman command did not answer." },
        },
    ])
}

fn suites() -> Value {
    json!([{
        "id": SUITE,
        "title": "Ogun sample suite",
        "adapter": "spider2_dbt",
        "tasks": ["sum-amounts", "count-rows", "largest-city", "network-denied"],
        "agents": ["reference-script", "idle-script"],
        "note": "Four small tasks and two scripted agents, not models. It checks the harness: a fresh workspace per run, the network denied (the last task passes only when a loopback listener this engine opened was not reached), an evaluator that scores the result, a hash-chained trajectory per run and lineage events. It says nothing about any model.",
    }])
}

fn summary_of(dir: &Path, meta: &Meta) -> Value {
    let results: Option<Value> = std::fs::read(dir.join("out").join("results.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let rows = results
        .as_ref()
        .and_then(|r| r.get("rows"))
        .and_then(Value::as_array);
    let total = rows.map_or(0, Vec::len);
    let passed = rows.map_or(0, |r| {
        r.iter()
            .filter(|x| x.get("passed").and_then(Value::as_bool) == Some(true))
            .count()
    });
    let isolation = rows
        .and_then(|r| r.first())
        .and_then(|x| x.get("isolation"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "id": meta.id,
        "suite": meta.suite,
        "runtime": meta.runtime,
        "state": meta.state,
        "started_at": meta.started_at,
        "finished_at": meta.finished_at,
        "runs": total,
        "passed": passed,
        "isolation": isolation,
        "probe_hits": meta.probe_hits,
        "error": meta.error,
    })
}

async fn list(d: &Arc<Daemon>) -> ApiResult<Value> {
    let doc = doctor(d).await;
    let runs: Vec<Value> = d
        .studio
        .bench
        .metas()
        .into_iter()
        .take(MAX_RUNS_LISTED)
        .map(|(dir, m)| summary_of(&dir, &m))
        .collect();
    out(&json!({
        "available": !doc.is_null(),
        "runtimes": runtimes(&doc),
        "suites": suites(),
        "adapters": doc.get("adapters").cloned().unwrap_or_else(|| json!([])),
        "runs": runs,
    }))
}

#[derive(Deserialize)]
struct RunParams {
    #[serde(default)]
    suite: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    seeds: Option<Vec<u64>>,
    #[serde(default)]
    #[allow(dead_code)]
    idempotency_key: Option<String>,
}

async fn run(d: &Arc<Daemon>, conn: &Conn, p: RunParams) -> ApiResult<Value> {
    let suite = p.suite.unwrap_or_else(|| SUITE.into());
    if suite != SUITE {
        return Err(ApiError::new(
            "bench.spec_invalid",
            "only the sample suite is available in this build",
        ));
    }
    let runtime = p.runtime.unwrap_or_else(|| "local".into());
    if !["local", "docker", "podman"].contains(&runtime.as_str()) {
        return Err(ApiError::new(
            "bench.spec_invalid",
            "runtime must be local, docker or podman",
        ));
    }
    let seeds = p.seeds.unwrap_or_else(|| vec![1]);
    if seeds.is_empty() || seeds.len() > MAX_SEEDS {
        return Err(ApiError::new(
            "bench.spec_invalid",
            format!("between 1 and {MAX_SEEDS} seeds"),
        ));
    }
    let doc = doctor(d).await;
    let available = runtimes(&doc)
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"] == runtime.as_str()).cloned())
        .and_then(|r| r["available"].as_bool())
        .unwrap_or(false);
    if !available {
        return Err(ApiError::new(
            "bench.isolation_unavailable",
            format!(
                "the {runtime} runtime cannot deny the network on this computer, so no run was started"
            ),
        ));
    }
    let exe = bench_exe()?;
    let bench = &d.studio.bench;
    let id = format!("bmr_{}", ulid::Ulid::generate());
    let dir = bench.runs_dir().join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| {
        ApiError::unavailable(format!("cannot create the run directory ({:?})", e.kind()))
    })?;
    std::fs::create_dir_all(bench.root.join("state")).map_err(|e| {
        ApiError::unavailable(format!(
            "cannot create the benchmark state ({:?})",
            e.kind()
        ))
    })?;

    // The network task tries this address; whatever it does, the number of connections accepted is recorded.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| {
            ApiError::unavailable(format!("cannot open the probe listener ({:?})", e.kind()))
        })?;
    let probe = listener
        .local_addr()
        .map_err(|e| ApiError::internal(e.to_string()))?
        .to_string();

    let mut generate = Command::new(&exe);
    harness_env(&mut generate, &bench.home, &bench.root);
    generate
        .arg("sample-matrix")
        .arg("--dir")
        .arg(&dir)
        .arg("--runtime")
        .arg(&runtime)
        .arg("--probe")
        .arg(&probe)
        .arg("--seeds")
        .arg(
            seeds
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(","),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let ok = tokio::time::timeout(Duration::from_secs(30), generate.status())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|s| s.success());
    if !ok {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(ApiError::new(
            "bench.environment_failed",
            "the sample suite could not be written",
        ));
    }

    let meta = Meta {
        id: id.clone(),
        suite,
        runtime: runtime.clone(),
        seeds,
        state: "running".into(),
        started_at: ogun_types::Timestamp::now().to_rfc3339(),
        finished_at: None,
        probe_hits: 0,
        error: None,
    };
    write_meta(&dir, &meta)?;

    let (cancel_tx, cancel_rx) = watch::channel(false);
    lock(&bench.live).insert(id.clone(), cancel_tx);
    tracing::info!(by = %conn.label(), run = %id, runtime = %runtime, "benchmark run started");
    tokio::spawn(supervise(
        Arc::clone(d),
        exe,
        dir,
        meta,
        listener,
        cancel_rx,
    ));
    out(&json!({ "id": id }))
}

async fn supervise(
    d: Arc<Daemon>,
    exe: PathBuf,
    dir: PathBuf,
    mut meta: Meta,
    listener: tokio::net::TcpListener,
    mut cancel: watch::Receiver<bool>,
) {
    let bench = &d.studio.bench;
    let log = std::fs::File::create(dir.join("harness.log")).ok();
    let mut cmd = Command::new(&exe);
    harness_env(&mut cmd, &bench.home, &bench.root);
    cmd.arg("--state-dir")
        .arg(bench.root.join("state"))
        .arg("run")
        .arg("--matrix")
        .arg(dir.join("matrix.json"))
        .arg("--out")
        .arg(dir.join("out"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    match log {
        Some(f) => cmd.stderr(Stdio::from(f)),
        None => cmd.stderr(Stdio::null()),
    };
    let mut hits = 0u64;
    let outcome: Result<std::process::ExitStatus, String> = match cmd.spawn() {
        Err(e) => Err(format!(
            "the benchmark harness could not start ({:?})",
            e.kind()
        )),
        Ok(mut child) => loop {
            tokio::select! {
                status = child.wait() => break status.map_err(|e| format!("waiting for the harness failed ({:?})", e.kind())),
                accepted = listener.accept() => { if accepted.is_ok() { hits += 1; } }
                _ = cancel.changed() => {
                    ogun_ops::sandbox::kill_tree(&mut child).await;
                    meta.state = "cancelled".into();
                    break child.wait().await.map_err(|e| format!("{:?}", e.kind()));
                }
            }
        },
    };
    meta.probe_hits = hits;
    meta.finished_at = Some(ogun_types::Timestamp::now().to_rfc3339());
    if meta.state != "cancelled" {
        match outcome {
            Ok(s) if s.success() => meta.state = "finished".into(),
            Ok(s) => {
                meta.state = "failed".into();
                meta.error = Some(format!(
                    "the harness exited with {s}; harness.log in the run folder has the details"
                ));
            }
            Err(e) => {
                meta.state = "failed".into();
                meta.error = Some(e);
            }
        }
    }
    let _ = write_meta(&dir, &meta);
    lock(&bench.live).remove(&meta.id);
    tracing::info!(run = %meta.id, state = %meta.state, probe_hits = hits, "benchmark run ended");
}

#[derive(Deserialize)]
struct IdParams {
    id: String,
}

fn status(d: &Daemon, p: &IdParams) -> ApiResult<Value> {
    let (dir, meta) = d.studio.bench.find(&p.id)?;
    let results: Value = std::fs::read(dir.join("out").join("results.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let markdown = std::fs::read_to_string(dir.join("out").join("results.md")).ok();
    let log_tail = std::fs::read_to_string(dir.join("harness.log"))
        .ok()
        .map(|t| {
            let lines: Vec<&str> = t.lines().collect();
            lines[lines.len().saturating_sub(20)..].join("\n")
        });
    let mut summary = summary_of(&dir, &meta);
    summary["seeds"] = json!(meta.seeds);
    out(&json!({
        "run": summary,
        "rows": results.get("rows").cloned().unwrap_or_else(|| json!([])),
        "agents": results.get("agents").cloned().unwrap_or_else(|| json!([])),
        "matrix_blake3": results.get("matrix_blake3").cloned().unwrap_or(Value::Null),
        "markdown": markdown,
        "log_tail": log_tail,
    }))
}

fn cancel(d: &Daemon, p: &IdParams) -> ApiResult<Value> {
    let (_, meta) = d.studio.bench.find(&p.id)?;
    let tx = lock(&d.studio.bench.live).get(&meta.id).cloned();
    match tx {
        Some(tx) => {
            let _ = tx.send(true);
            out(&json!({ "cancelled": true }))
        }
        None => out(&json!({ "cancelled": false, "state": meta.state })),
    }
}

#[derive(Deserialize)]
struct ReplayParams {
    id: String,
    agent_run: String,
}

async fn replay(d: &Arc<Daemon>, p: ReplayParams) -> ApiResult<Value> {
    let (dir, _) = d.studio.bench.find(&p.id)?;
    if !p.agent_run.starts_with("agr_") || p.agent_run.contains(['/', '\\', '.']) {
        return Err(ApiError::new("api.invalid_params", "not an agent run id"));
    }
    let path = dir
        .join("out")
        .join("runs")
        .join(&p.agent_run)
        .join("trajectory.jsonl");
    if !path.is_file() {
        return Err(ApiError::new(
            "api.invalid_params",
            "that run has no trajectory",
        ));
    }
    let mut cmd = Command::new(bench_exe()?);
    harness_env(&mut cmd, &d.studio.bench.home, &d.studio.bench.root);
    cmd.arg("replay")
        .arg("--summary")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let o = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| {
            ApiError::new(
                "bench.environment_failed",
                "verifying the trajectory timed out",
            )
        })?
        .map_err(|e| {
            ApiError::new(
                "bench.environment_failed",
                format!("cannot run ogun-bench ({:?})", e.kind()),
            )
        })?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let events: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .take(400)
        .map(|v| {
            let mut m = v.as_object().cloned().unwrap_or_default();
            m.remove("agent_run");
            m.remove("prev_blake3");
            Value::Object(m)
        })
        .collect();
    let error = (!o.status.success()).then(|| {
        String::from_utf8_lossy(&o.stderr)
            .trim()
            .chars()
            .take(300)
            .collect::<String>()
    });
    out(&json!({
        "verified": o.status.success(),
        "summary": serde_json::from_slice::<Value>(&o.stdout).ok(),
        "error": error,
        "events": events,
    }))
}

/// Routes `bench.*`.
pub async fn dispatch(
    d: &Arc<Daemon>,
    conn: &Conn,
    method: &str,
    p: Value,
) -> Option<ApiResult<Value>> {
    Some(match method {
        "bench.list" => list(d).await,
        "bench.run" => match args::<RunParams>(p) {
            Ok(a) => run(d, conn, a).await,
            Err(e) => Err(e),
        },
        "bench.status" => args::<IdParams>(p).and_then(|a| status(d, &a)),
        "bench.cancel" => args::<IdParams>(p).and_then(|a| cancel(d, &a)),
        "bench.replay" => match args::<ReplayParams>(p) {
            Ok(a) => replay(d, a).await,
            Err(e) => Err(e),
        },
        _ => return None,
    })
}
