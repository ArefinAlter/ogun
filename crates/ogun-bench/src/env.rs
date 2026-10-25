//! Environments (`env_`, O-06 section 4).
//!
//! An environment is created fresh for every agent run and has a directory of its own under the benchmark state
//! directory:
//!
//! ```text
//! <state>/envs/<env_id>/
//!   environment.json   what `ogun-bench exec` needs to run a command in it
//!   workspace/         read-write; mounted at the task's working directory (the agent's work)
//!   benchmark/         read-only; instruction.md and the files the task lets the agent read
//!   logs/agent/        read-write; the agent's own logs and ogun-trajectory.jsonl
//!   logs/verifier/     read-write; the evaluator's output (Harbor's reward.txt)
//!   tests/             staged only after the agent finished (local runtime)
//!   cache/             only for cache modes cold and warm (warm uses <state>/warm/<task>)
//! ```
//!
//! Container runtimes (`docker`, `podman`) run one long-lived container per environment with every capability
//! dropped, `no-new-privileges`, a process limit, optional CPU and memory limits, and either no network or an
//! internal network without egress. Commands run with `exec`. The local runtime runs commands as child processes
//! with a cleared environment in the workspace; it denies the network with a user and network namespace
//! (`unshare -r -n`) and refuses to run when it cannot, unless a person set `allow_unisolated_local`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use ogun_types::{EnvironmentId, IdGenerator};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::task::Task;
use crate::workspace;
use crate::{BenchError, BenchResult, jcs_blake3};

/// Most bytes kept from one stream of one command; the rest is counted and dropped.
pub const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

/// Which runtime isolates the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    /// The `docker` CLI.
    #[default]
    Docker,
    /// The `podman` CLI.
    Podman,
    /// A local temporary directory and child processes (development and trusted tasks only).
    Local,
}

impl Runtime {
    /// The CLI program, for container runtimes.
    #[must_use]
    pub const fn cli(self) -> Option<&'static str> {
        match self {
            Self::Docker => Some("docker"),
            Self::Podman => Some("podman"),
            Self::Local => None,
        }
    }

    const fn default_network(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            _ => "bridge",
        }
    }
}

/// Network access for one phase of a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// No egress. The default (ADR 0005 decision 3).
    #[default]
    Denied,
    /// Egress allowed; only when a person set it for the matrix.
    Allowed,
}

/// What caches the environment offers the agent (ADR 0005 decision 2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    /// No cache directory.
    #[default]
    Disabled,
    /// Only what the task ships, read-only at `/cache`.
    BenchmarkProvided,
    /// An empty `/cache` for this run.
    Cold,
    /// A `/cache` kept across runs of the same task and environment digest.
    Warm,
}

fn default_pids() -> u32 {
    512
}

/// How environments are built for a matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSpec {
    /// Runtime.
    #[serde(default)]
    pub runtime: Runtime,
    /// Network while the agent works.
    #[serde(default)]
    pub agent_network: NetworkPolicy,
    /// Network while the evaluator runs (Harbor's `test.sh` usually installs its test runner from the network).
    #[serde(default)]
    pub evaluator_network: NetworkPolicy,
    /// CPU limit; the task's own limit applies when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// Memory limit in MiB; the task's own limit applies when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    /// Process limit.
    #[serde(default = "default_pids")]
    pub pids_limit: u32,
    /// Agent caches.
    #[serde(default)]
    pub cache_mode: CacheMode,
    /// An image for tasks that name none (Spider 2.0-DBT and ADE-bench need dbt and DuckDB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Run local environments without a network namespace when none can be made. Off by default; results of such
    /// runs record `isolation: unisolated_local`.
    #[serde(default)]
    pub allow_unisolated_local: bool,
}

impl Default for EnvironmentSpec {
    fn default() -> Self {
        Self {
            runtime: Runtime::Docker,
            agent_network: NetworkPolicy::Denied,
            evaluator_network: NetworkPolicy::Denied,
            cpus: None,
            memory_mb: None,
            pids_limit: default_pids(),
            cache_mode: CacheMode::Disabled,
            image: None,
            allow_unisolated_local: false,
        }
    }
}

/// What `ogun-bench exec` reads to run a command in an existing environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvRecord {
    /// The environment.
    pub id: EnvironmentId,
    /// Runtime.
    pub runtime: Runtime,
    /// Container name, for container runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Working directory (a container path, or the host workspace).
    pub workdir: String,
    /// Host workspace.
    pub workspace: PathBuf,
    /// Host logs directory.
    pub logs: PathBuf,
    /// Local runtime: wrap commands in `unshare -r -n`.
    #[serde(default)]
    pub local_netns: bool,
}

/// One command's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    /// Exit code; `None` when killed.
    pub exit_code: Option<i32>,
    /// Standard output (at most [`MAX_CAPTURE_BYTES`]).
    pub stdout: Vec<u8>,
    /// Standard error (at most [`MAX_CAPTURE_BYTES`]).
    pub stderr: Vec<u8>,
    /// Wall time.
    pub duration_ms: u64,
    /// Stopped at its time limit.
    pub timed_out: bool,
}

/// Options for one command.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Non-secret variables.
    pub env: BTreeMap<String, String>,
    /// Names of variables passed through from the harness's own environment (model credentials). Their values
    /// never appear on a command line.
    pub pass_env: Vec<String>,
    /// Time limit.
    pub timeout_ms: u64,
}

/// Paths a command template may name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplatePaths {
    /// `{workspace}`.
    pub workspace: String,
    /// `{logs}`.
    pub logs: String,
    /// `{tests}`.
    pub tests: String,
    /// `{benchmark}`.
    pub benchmark: String,
    /// `{instruction_file}`.
    pub instruction_file: String,
    /// `{cache}`.
    pub cache: String,
}

/// Replaces `{name}` placeholders in every argument.
#[must_use]
pub fn expand(argv: &[String], vars: &BTreeMap<&str, String>) -> Vec<String> {
    argv.iter()
        .map(|a| {
            let mut s = a.clone();
            for (k, v) in vars {
                s = s.replace(&format!("{{{k}}}"), v);
            }
            s
        })
        .collect()
}

/// A live environment.
#[derive(Debug)]
pub struct Environment {
    /// Its id.
    pub id: EnvironmentId,
    /// The spec it was built from.
    pub spec: EnvironmentSpec,
    /// Its directory.
    pub dir: PathBuf,
    /// Host workspace.
    pub workspace: PathBuf,
    /// Host logs.
    pub logs: PathBuf,
    /// Host benchmark directory.
    pub benchmark: PathBuf,
    /// Host tests directory (local runtime).
    pub tests: PathBuf,
    /// Host cache directory, when the cache mode has one.
    pub cache: Option<PathBuf>,
    /// The workspace as commands see it.
    pub workdir: String,
    /// BLAKE3 over the environment description (O-06 section 4.3).
    pub digest: String,
    /// `container`, `local_netns` or `unisolated_local`.
    pub isolation: &'static str,
    container: Option<String>,
    network: Option<String>,
    local_netns: bool,
}

/// The user's Ogun and Zeus state directories, which a benchmark must never touch.
#[must_use]
pub fn protected_dirs() -> Vec<(&'static str, PathBuf)> {
    let mut out = vec![];
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = env("OGUN_HOME") {
        out.push(("the Ogun state directory", p));
    }
    if let Some(p) = env("ZEUS_STATE_DIR") {
        out.push(("the Zeus state directory", p));
    }
    if let Some(base) = directories::BaseDirs::new() {
        let state = base
            .state_dir()
            .unwrap_or_else(|| base.data_local_dir())
            .to_path_buf();
        let (ogun, zeus) = if cfg!(windows) {
            (
                base.data_local_dir().join("Ogun"),
                base.data_local_dir().join("Zeus"),
            )
        } else if cfg!(target_os = "macos") {
            (base.data_dir().join("Ogun"), base.data_dir().join("Zeus"))
        } else {
            (state.join("ogun"), state.join("zeus"))
        };
        out.push(("the Ogun state directory", ogun));
        out.push(("the Zeus state directory", zeus));
    }
    out
}

// Resolve existing symlink prefixes even when the requested directory does not exist yet.
// Comparing raw paths after a failed canonicalize would miss `missing/../ogun` and relative paths.
fn state_path(p: &Path) -> BenchResult<PathBuf> {
    let absolute = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(BenchError::io("resolve the state directory"))?
            .join(p)
    };
    let mut resolved = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            _ => {
                resolved.push(part.as_os_str());
                match std::fs::canonicalize(&resolved) {
                    Ok(path) => resolved = path,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(BenchError::io("resolve the state directory")(e)),
                }
            }
        }
    }
    Ok(resolved)
}

/// Refuses a benchmark state directory that is inside, or contains, the user's Ogun or Zeus state.
///
/// # Errors
///
/// [`BenchError::StoreOverlap`].
pub fn check_isolated_state(state_root: &Path) -> BenchResult<()> {
    let ours = state_path(state_root)?;
    for (what, p) in protected_dirs() {
        let theirs = state_path(&p)?;
        if ours.starts_with(&theirs) || theirs.starts_with(&ours) {
            return Err(BenchError::StoreOverlap(what));
        }
    }
    Ok(())
}

async fn run_cli(program: &str, args: &[&str], timeout_ms: u64) -> BenchResult<(bool, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| BenchError::Environment(format!("cannot start {program}: {:?}", e.kind())))?;
    let out = tokio::time::timeout(
        Duration::from_millis(timeout_ms.max(1)),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| {
        BenchError::Environment(format!(
            "{program} {} timed out",
            args.first().unwrap_or(&"")
        ))
    })?
    .map_err(|e| BenchError::Environment(format!("{program} failed: {:?}", e.kind())))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        tracing::debug!(%program, cmd = args.first().copied().unwrap_or(""), stderr = %err.chars().take(400).collect::<String>(), "runtime command failed");
    }
    Ok((out.status.success(), text))
}

async fn must(program: &str, args: &[&str], timeout_ms: u64) -> BenchResult<String> {
    match run_cli(program, args, timeout_ms).await? {
        (true, out) => Ok(out),
        (false, _) => Err(BenchError::Environment(format!(
            "{program} {} failed",
            args.first().unwrap_or(&"")
        ))),
    }
}

/// A Docker-safe image tag for a task built from its own context.
fn build_tag(task: &Task) -> String {
    let name: String = format!("{}-{}", task.benchmark, task.id)
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(100)
        .collect();
    format!(
        "ogun-bench/{name}:{}",
        &task.task_blake3[..12.min(task.task_blake3.len())]
    )
}

async fn read_bounded(mut r: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_CAPTURE_BYTES.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    out
}

/// Runs a prepared command with bounded capture and a time limit. On timeout the process (and, on Unix, its
/// process group) is killed.
async fn run_command(mut cmd: Command, timeout_ms: u64) -> BenchResult<ExecOutput> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| {
        BenchError::Environment(format!("cannot start the command: {:?}", e.kind()))
    })?;
    let pid = child.id();
    let out = child.stdout.take().map(|s| tokio::spawn(read_bounded(s)));
    let err = child.stderr.take().map(|s| tokio::spawn(read_bounded(s)));
    let waited = tokio::time::timeout(Duration::from_millis(timeout_ms.max(1)), child.wait()).await;
    let (exit_code, timed_out) = match waited {
        Ok(Ok(status)) => (status.code(), false),
        Ok(Err(e)) => {
            return Err(BenchError::Environment(format!(
                "waiting failed: {:?}",
                e.kind()
            )));
        }
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = pid {
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = child.kill().await;
            (None, true)
        }
    };
    #[cfg(not(unix))]
    let _ = pid;
    let stdout = match out {
        Some(h) => h.await.unwrap_or_default(),
        None => vec![],
    };
    let stderr = match err {
        Some(h) => h.await.unwrap_or_default(),
        None => vec![],
    };
    Ok(ExecOutput {
        exit_code,
        stdout,
        stderr,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        timed_out,
    })
}

/// Whether `unshare -r -n` works here (Linux with unprivileged user namespaces).
async fn netns_available() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    matches!(
        run_cli("unshare", &["-r", "-n", "true"], 10_000).await,
        Ok((true, _))
    )
}

fn local_env(record: &EnvRecord, opts: &ExecOptions) -> Vec<(String, String)> {
    let tmp = record.logs.join("tmp").display().to_string();
    let mut vars = vec![
        (
            "PATH".to_owned(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
        ),
        ("HOME".to_owned(), record.workspace.display().to_string()),
        ("LANG".to_owned(), "C.UTF-8".to_owned()),
        ("TMPDIR".to_owned(), tmp.clone()),
    ];
    if cfg!(windows) {
        // Windows runtimes need the OS location even in a cleared environment.
        // Keep the same narrow platform defaults as the operator runner; Windows
        // environment-variable lookups are case-insensitive.
        for name in ["SystemRoot", "windir"] {
            if let Ok(value) = std::env::var(name) {
                vars.push((name.into(), value));
            }
        }
        vars.push(("TEMP".into(), tmp.clone()));
        vars.push(("TMP".into(), tmp));
    }
    for name in &opts.pass_env {
        if let Ok(v) = std::env::var(name) {
            vars.push((name.clone(), v));
        }
    }
    vars.extend(opts.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    vars
}

/// Runs a command in an environment described by its record. Used by [`Environment::exec`] and by
/// `ogun-bench exec`, the shim host-placed agents use to run commands in the environment.
///
/// # Errors
///
/// [`BenchError::Environment`] when the command cannot be started.
pub async fn exec_in(
    record: &EnvRecord,
    argv: &[String],
    opts: &ExecOptions,
) -> BenchResult<ExecOutput> {
    let (first, rest) = argv
        .split_first()
        .ok_or_else(|| BenchError::Spec("an empty command".into()))?;
    match (record.runtime.cli(), &record.container) {
        (Some(cli), Some(container)) => {
            let mut cmd = Command::new(cli);
            cmd.arg("exec").arg("-w").arg(&record.workdir);
            for (k, v) in &opts.env {
                cmd.arg("-e").arg(format!("{k}={v}"));
            }
            for name in &opts.pass_env {
                // `-e NAME` without a value: the CLI copies it from its own environment, so the value never
                // appears on a command line.
                cmd.arg("-e").arg(name);
            }
            cmd.arg(container).arg(first).args(rest);
            let out = run_command(cmd, opts.timeout_ms).await?;
            if out.timed_out {
                // Stop whatever the command left running inside; the file system stays.
                let _ = cli_restart(cli, container).await;
            }
            Ok(out)
        }
        _ => {
            let mut cmd = if record.local_netns {
                let mut c = Command::new("unshare");
                c.arg("-r").arg("-n").arg(first).args(rest);
                c
            } else {
                let mut c = Command::new(first);
                c.args(rest);
                c
            };
            std::fs::create_dir_all(record.logs.join("tmp"))
                .map_err(BenchError::io("create the temporary directory"))?;
            cmd.env_clear()
                .envs(local_env(record, opts))
                .current_dir(&record.workspace);
            run_command(cmd, opts.timeout_ms).await
        }
    }
}

async fn cli_restart(cli: &str, container: &str) -> BenchResult<()> {
    must(cli, &["restart", "-t", "0", container], 60_000)
        .await
        .map(|_| ())
}

impl Environment {
    /// Creates a fresh environment for one agent run of `task`.
    ///
    /// # Errors
    ///
    /// [`BenchError::StoreOverlap`], [`BenchError::IsolationUnavailable`], [`BenchError::Environment`] (image
    /// build or container start failed), [`BenchError::Io`].
    pub async fn create(
        spec: &EnvironmentSpec,
        task: &Task,
        state_root: &Path,
        ids: &IdGenerator,
    ) -> BenchResult<Self> {
        check_isolated_state(state_root)?;
        let id = ids.environment_id();
        let dir = state_root.join("envs").join(id.to_string());
        let workspace = dir.join("workspace");
        let logs = dir.join("logs");
        let benchmark = dir.join("benchmark");
        let tests = dir.join("tests");
        for d in [
            &workspace,
            &logs.join("agent"),
            &logs.join("verifier"),
            &benchmark,
        ] {
            std::fs::create_dir_all(d)
                .map_err(BenchError::io("create the environment directories"))?;
        }
        let cache = match spec.cache_mode {
            CacheMode::Disabled => None,
            CacheMode::BenchmarkProvided => {
                let c = task.dir.join("cache");
                c.is_dir().then_some(c)
            }
            CacheMode::Cold => Some(dir.join("cache")),
            CacheMode::Warm => Some(state_root.join("warm").join(&task.task_blake3)),
        };
        if let (Some(c), CacheMode::Cold | CacheMode::Warm) = (&cache, spec.cache_mode) {
            std::fs::create_dir_all(c).map_err(BenchError::io("create the cache directory"))?;
        }

        let mut me = Self {
            id,
            spec: spec.clone(),
            dir: dir.clone(),
            workspace: workspace.clone(),
            logs: logs.clone(),
            benchmark: benchmark.clone(),
            tests,
            cache,
            workdir: String::new(),
            digest: String::new(),
            isolation: "container",
            container: None,
            network: None,
            local_netns: false,
        };

        let image_id = match spec.runtime.cli() {
            Some(cli) => {
                let image = me.ensure_image(cli, task).await?;
                me.workdir = task.environment.workdir.clone();
                if task.environment.workdir_from_image
                    && let Ok((true, wd)) = run_cli(
                        cli,
                        &[
                            "image",
                            "inspect",
                            "--format",
                            "{{.Config.WorkingDir}}",
                            &image,
                        ],
                        30_000,
                    )
                    .await
                    && wd.starts_with('/')
                {
                    me.workdir = wd;
                }
                me.seed_workspace(cli, &image).await;
                Some((cli, image))
            }
            None => {
                me.workdir = workspace.display().to_string();
                let need_netns = spec.agent_network == NetworkPolicy::Denied
                    || spec.evaluator_network == NetworkPolicy::Denied;
                if need_netns {
                    if netns_available().await {
                        me.local_netns = spec.agent_network == NetworkPolicy::Denied;
                        me.isolation = "local_netns";
                    } else if spec.allow_unisolated_local {
                        tracing::warn!(env = %me.id, "local environment without a network namespace");
                        me.isolation = "unisolated_local";
                    } else {
                        return Err(BenchError::IsolationUnavailable(
                            "the local runtime cannot deny the network here (no unprivileged network namespace); \
                             use a container runtime or set allow_unisolated_local"
                                .into(),
                        ));
                    }
                } else {
                    me.isolation = "unisolated_local";
                }
                None
            }
        };

        for (src, sub) in &task.fixtures {
            let target = if sub.is_empty() {
                workspace.clone()
            } else {
                workspace.join(sub)
            };
            if src.is_file() {
                std::fs::create_dir_all(&target)
                    .map_err(BenchError::io("create a fixture directory"))?;
                let name = src
                    .file_name()
                    .ok_or_else(|| BenchError::Spec("a fixture file has no name".into()))?;
                std::fs::copy(src, target.join(name))
                    .map_err(BenchError::io("copy a fixture file"))?;
            } else {
                workspace::copy_dir(src, &target)?;
            }
        }
        if let Some(b) = &task.benchmark_files {
            workspace::copy_dir(b, &benchmark)?;
        }
        std::fs::write(
            benchmark.join("instruction.md"),
            task.instruction.as_bytes(),
        )
        .map_err(BenchError::io("write the instruction"))?;
        workspace::make_read_only(&benchmark);
        let fixture_digest = workspace::tree_digest(&workspace::snapshot(&workspace, &[])?);

        let image_digest = match &image_id {
            Some((cli, image)) => {
                let id = must(
                    cli,
                    &["image", "inspect", "--format", "{{.Id}}", image],
                    30_000,
                )
                .await?;
                me.start_container(cli, image, task).await?;
                Some(id)
            }
            None => None,
        };
        me.digest = jcs_blake3(&serde_json::json!({
            "runtime": spec.runtime,
            "image_id": image_digest,
            "workdir": if spec.runtime == Runtime::Local { "<workspace>".to_owned() } else { me.workdir.clone() },
            "agent_network": spec.agent_network,
            "evaluator_network": spec.evaluator_network,
            "cpus": spec.cpus.or(task.environment.cpus),
            "memory_mb": spec.memory_mb.or(task.environment.memory_mb),
            "pids_limit": spec.pids_limit,
            "cache_mode": spec.cache_mode,
            "isolation": me.isolation,
            "fixture_blake3": fixture_digest,
        }));
        let record = me.record();
        let text = serde_json::to_vec_pretty(&record)
            .map_err(|e| BenchError::Environment(e.to_string()))?;
        std::fs::write(dir.join("environment.json"), text)
            .map_err(BenchError::io("write environment.json"))?;
        tracing::info!(env = %me.id, isolation = me.isolation, task = %task.id, "environment ready");
        Ok(me)
    }

    async fn ensure_image(&self, cli: &str, task: &Task) -> BenchResult<String> {
        if let Some(image) = self
            .spec
            .image
            .clone()
            .or_else(|| task.environment.image.clone())
        {
            if !matches!(cli_ok(cli, &["image", "inspect", &image]).await, Ok(true)) {
                must(cli, &["pull", &image], 1_800_000).await?;
            }
            return Ok(image);
        }
        let Some(ctx) = &task.environment.build_context else {
            return Err(BenchError::TaskUnsupported {
                task: task.id.clone(),
                reason: "no image and no Dockerfile; set `image` in the environment spec".into(),
            });
        };
        let tag = build_tag(task);
        if matches!(cli_ok(cli, &["image", "inspect", &tag]).await, Ok(true)) {
            return Ok(tag);
        }
        let ctx = ctx
            .to_str()
            .ok_or_else(|| BenchError::Spec("build context is not UTF-8".into()))?;
        tracing::info!(task = %task.id, %tag, "building the task image");
        must(
            cli,
            &["build", "--label", "ogun.bench=1", "-t", &tag, ctx],
            task.environment.build_timeout_ms.unwrap_or(1_800_000),
        )
        .await?;
        Ok(tag)
    }

    /// Copies the image's working directory into the host workspace, so a task whose files live in the image
    /// (Harbor) still has a host workspace to mount, snapshot and diff.
    async fn seed_workspace(&self, cli: &str, image: &str) {
        let seed = format!("ogun-seed-{}", self.id.to_string().to_ascii_lowercase());
        if !matches!(
            cli_ok(cli, &["create", "--name", &seed, image]).await,
            Ok(true)
        ) {
            return;
        }
        let from = format!("{seed}:{}/.", self.workdir.trim_end_matches('/'));
        let to = self.workspace.display().to_string();
        let _ = cli_ok(cli, &["cp", &from, &to]).await;
        let _ = cli_ok(cli, &["rm", "-f", &seed]).await;
    }

    async fn start_container(&mut self, cli: &str, image: &str, task: &Task) -> BenchResult<()> {
        let name = format!("ogun-bench-{}", self.id.to_string().to_ascii_lowercase());
        let network = match (self.spec.agent_network, self.spec.evaluator_network) {
            (NetworkPolicy::Allowed, _) => self.spec.runtime.default_network().to_owned(),
            (NetworkPolicy::Denied, NetworkPolicy::Denied) => "none".to_owned(),
            (NetworkPolicy::Denied, NetworkPolicy::Allowed) => {
                // An internal network has no egress; the evaluator phase connects the default network as well.
                let net = format!("{name}-net");
                must(
                    cli,
                    &[
                        "network",
                        "create",
                        "--internal",
                        "--label",
                        "ogun.bench=1",
                        &net,
                    ],
                    60_000,
                )
                .await?;
                self.network = Some(net.clone());
                net
            }
        };
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            name.clone(),
            "--label".into(),
            "ogun.bench=1".into(),
            "--network".into(),
            network,
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--pids-limit".into(),
            self.spec.pids_limit.to_string(),
            "-w".into(),
            self.workdir.clone(),
            "-v".into(),
            format!("{}:{}:rw", self.workspace.display(), self.workdir),
            "-v".into(),
            format!("{}:/logs:rw", self.logs.display()),
            "-v".into(),
            format!("{}:/benchmark:ro", self.benchmark.display()),
        ];
        if let Some(c) = &self.cache {
            let mode = if self.spec.cache_mode == CacheMode::BenchmarkProvided {
                "ro"
            } else {
                "rw"
            };
            args.push("-v".into());
            args.push(format!("{}:/cache:{mode}", c.display()));
        }
        if let Some(cpus) = self.spec.cpus.or(task.environment.cpus) {
            args.push("--cpus".into());
            args.push(cpus.to_string());
        }
        if let Some(mb) = self.spec.memory_mb.or(task.environment.memory_mb) {
            args.push("--memory".into());
            args.push(format!("{mb}m"));
        }
        args.extend([
            "--entrypoint".into(),
            "/bin/sh".into(),
            image.to_owned(),
            "-c".into(),
            "trap 'exit 0' TERM INT; while :; do sleep 3600 & wait $!; done".into(),
        ]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        must(cli, &refs, 300_000).await?;
        self.container = Some(name);
        Ok(())
    }

    /// What `ogun-bench exec` needs.
    #[must_use]
    pub fn record(&self) -> EnvRecord {
        EnvRecord {
            id: self.id,
            runtime: self.spec.runtime,
            container: self.container.clone(),
            workdir: self.workdir.clone(),
            workspace: self.workspace.clone(),
            logs: self.logs.clone(),
            local_netns: self.local_netns,
        }
    }

    /// Reads `environment.json` from an environment directory.
    ///
    /// # Errors
    ///
    /// [`BenchError::Io`] or [`BenchError::Spec`].
    pub fn load_record(dir: &Path) -> BenchResult<EnvRecord> {
        let text = std::fs::read(dir.join("environment.json"))
            .map_err(BenchError::io("read environment.json"))?;
        serde_json::from_slice(&text)
            .map_err(|_| BenchError::Spec("environment.json is malformed".into()))
    }

    /// Paths for command templates, as commands inside the environment see them.
    #[must_use]
    pub fn paths(&self) -> TemplatePaths {
        if self.container.is_some() {
            TemplatePaths {
                workspace: self.workdir.clone(),
                logs: "/logs".into(),
                tests: "/tests".into(),
                benchmark: "/benchmark".into(),
                instruction_file: "/benchmark/instruction.md".into(),
                cache: "/cache".into(),
            }
        } else {
            self.host_paths()
        }
    }

    /// Paths for command templates, as a process on the host sees them (host-placed agents).
    #[must_use]
    pub fn host_paths(&self) -> TemplatePaths {
        TemplatePaths {
            workspace: self.workspace.display().to_string(),
            logs: self.logs.display().to_string(),
            tests: self.tests.display().to_string(),
            benchmark: self.benchmark.display().to_string(),
            instruction_file: self.benchmark.join("instruction.md").display().to_string(),
            cache: self
                .cache
                .as_ref()
                .map(|c| c.display().to_string())
                .unwrap_or_default(),
        }
    }

    /// Runs a command in the environment.
    ///
    /// # Errors
    ///
    /// As [`exec_in`].
    pub async fn exec(&self, argv: &[String], opts: &ExecOptions) -> BenchResult<ExecOutput> {
        exec_in(&self.record(), argv, opts).await
    }

    /// Stages the evaluator's files (`/tests`), replacing anything the agent may have put there.
    ///
    /// # Errors
    ///
    /// [`BenchError::Environment`] or [`BenchError::Io`].
    pub async fn stage_eval_files(&self, task: &Task) -> BenchResult<()> {
        let Some(src) = &task.eval_files else {
            return Ok(());
        };
        match (self.spec.runtime.cli(), &self.container) {
            (Some(cli), Some(c)) => {
                let rm: Vec<String> = ["rm", "-rf", "/tests"].map(String::from).to_vec();
                let opts = ExecOptions {
                    timeout_ms: 60_000,
                    ..ExecOptions::default()
                };
                exec_in(&self.record(), &rm, &opts).await?;
                let from = format!("{}/.", src.display());
                let to = format!("{c}:/tests");
                must(cli, &["cp", &from, &to], 300_000).await?;
            }
            _ => {
                if self.tests.exists() {
                    std::fs::remove_dir_all(&self.tests)
                        .map_err(BenchError::io("clear the tests directory"))?;
                }
                workspace::copy_dir(src, &self.tests)?;
            }
        }
        Ok(())
    }

    /// Runs the task's setup script in the workspace (copied in, run, removed), before the agent starts.
    ///
    /// # Errors
    ///
    /// [`BenchError::Environment`] when the script cannot run or exits non-zero.
    pub async fn run_setup(&self, task: &Task) -> BenchResult<()> {
        let Some(script) = &task.setup_script else {
            return Ok(());
        };
        let host = self.workspace.join(".ogun-setup.sh");
        std::fs::copy(script, &host).map_err(BenchError::io("stage the setup script"))?;
        let inside = format!("{}/.ogun-setup.sh", self.paths().workspace);
        let out = self
            .exec(
                &["sh".to_owned(), inside],
                &ExecOptions {
                    timeout_ms: 600_000,
                    ..ExecOptions::default()
                },
            )
            .await;
        let _ = std::fs::remove_file(&host);
        match out? {
            o if o.exit_code == Some(0) => Ok(()),
            o => Err(BenchError::Environment(format!(
                "the task setup script failed (exit code {:?})",
                o.exit_code
            ))),
        }
    }

    /// Applies the evaluator's network policy, including closing egress an agent was allowed.
    ///
    /// # Errors
    ///
    /// [`BenchError::Environment`].
    pub async fn open_evaluator_network(&mut self) -> BenchResult<()> {
        if self.spec.runtime == Runtime::Local {
            self.local_netns = self.isolation == "local_netns"
                && self.spec.evaluator_network == NetworkPolicy::Denied;
            return Ok(());
        }
        if self.spec.agent_network == self.spec.evaluator_network {
            return Ok(());
        }
        if let (Some(cli), Some(container)) = (self.spec.runtime.cli(), &self.container) {
            let action = if self.spec.evaluator_network == NetworkPolicy::Allowed {
                "connect"
            } else {
                "disconnect"
            };
            must(
                cli,
                &[
                    "network",
                    action,
                    self.spec.runtime.default_network(),
                    container,
                ],
                60_000,
            )
            .await?;
        }
        Ok(())
    }

    /// Removes the container and its network. The directory stays until the caller removes it.
    pub async fn teardown(&self) {
        if let Some(cli) = self.spec.runtime.cli() {
            if let Some(c) = &self.container {
                let _ = cli_ok(cli, &["rm", "-f", c]).await;
            }
            if let Some(n) = &self.network {
                let _ = cli_ok(cli, &["network", "rm", n]).await;
            }
        }
    }

    /// Removes the environment directory (after the caller copied what it keeps).
    pub fn remove_dir(&self) {
        // Read-only files under benchmark/ would make removal fail on some systems; make them writable first.
        let mut stack = vec![self.dir.clone()];
        while let Some(p) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push(e.path());
                    } else if let Ok(meta) = e.metadata() {
                        let mut perm = meta.permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        perm.set_readonly(false);
                        let _ = std::fs::set_permissions(e.path(), perm);
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn cli_ok(program: &str, args: &[&str]) -> BenchResult<bool> {
    run_cli(program, args, 120_000).await.map(|(ok, _)| ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_expand_and_store_overlap_is_refused() {
        let mut vars = BTreeMap::new();
        vars.insert("workspace", "/app".to_owned());
        assert_eq!(
            expand(&["cd {workspace} && ls".to_owned()], &vars),
            ["cd /app && ls"]
        );
        if let Some((_, ogun)) = protected_dirs().into_iter().next() {
            assert!(check_isolated_state(&ogun.join("bench")).is_err());
        }
    }

    #[test]
    fn nonexistent_state_paths_cannot_hide_protected_overlap() {
        for (_, protected) in protected_dirs() {
            let (Some(parent), Some(name)) = (protected.parent(), protected.file_name()) else {
                continue;
            };
            let disguised = parent
                .join("not-created")
                .join("..")
                .join(name)
                .join("bench");
            assert!(matches!(
                check_isolated_state(&disguised),
                Err(BenchError::StoreOverlap(_))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn nonexistent_paths_resolve_existing_symlink_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("link")).unwrap();
        let disguised = dir.path().join("link/not-created/../bench");
        assert_eq!(
            state_path(&disguised).unwrap(),
            std::fs::canonicalize(target).unwrap().join("bench")
        );
    }

    #[tokio::test]
    #[ignore = "requires Podman and OGUN_BENCH_PODMAN_IMAGE naming a local Linux image with sh and awk"]
    async fn podman_enforces_both_phase_network_policies() {
        let image = std::env::var("OGUN_BENCH_PODMAN_IMAGE").expect("set the test image");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("benchmark");
        std::fs::create_dir_all(root.join("examples/t1")).unwrap();
        std::fs::create_dir_all(root.join("evaluation_suite")).unwrap();
        std::fs::write(
            root.join("examples/spider2-dbt.jsonl"),
            "{\"instance_id\":\"t1\",\"instruction\":\"network smoke\"}\n",
        )
        .unwrap();
        let task = crate::adapters::spider2::load(&root).unwrap().remove(0);
        for (agent, evaluator) in [
            (NetworkPolicy::Denied, NetworkPolicy::Denied),
            (NetworkPolicy::Denied, NetworkPolicy::Allowed),
            (NetworkPolicy::Allowed, NetworkPolicy::Denied),
        ] {
            let spec = EnvironmentSpec {
                runtime: Runtime::Podman,
                image: Some(image.clone()),
                agent_network: agent,
                evaluator_network: evaluator,
                ..EnvironmentSpec::default()
            };
            let mut env =
                Environment::create(&spec, &task, &dir.path().join("state"), &IdGenerator::new())
                    .await
                    .unwrap();
            let route = vec![
                "awk".into(),
                "$2 == \"00000000\" { found=1 } END { exit !found }".into(),
                "/proc/net/route".into(),
            ];
            let opts = ExecOptions {
                timeout_ms: 10_000,
                ..Default::default()
            };
            let before = env.exec(&route, &opts).await;
            let transition = env.open_evaluator_network().await;
            let after = env.exec(&route, &opts).await;
            env.teardown().await;
            env.remove_dir();
            transition.unwrap();
            assert_eq!(
                before.unwrap().exit_code,
                Some(if agent == NetworkPolicy::Allowed {
                    0
                } else {
                    1
                })
            );
            assert_eq!(
                after.unwrap().exit_code,
                Some(if evaluator == NetworkPolicy::Allowed {
                    0
                } else {
                    1
                })
            );
        }
    }

    #[tokio::test]
    async fn local_commands_see_a_cleared_environment() {
        let dir = tempfile::tempdir().unwrap();
        let record = EnvRecord {
            id: IdGenerator::new().environment_id(),
            runtime: Runtime::Local,
            container: None,
            workdir: dir.path().display().to_string(),
            workspace: dir.path().to_path_buf(),
            logs: dir.path().join("logs"),
            local_netns: false,
        };
        let out = exec_in(
            &record,
            &[
                "sh".into(),
                "-c".into(),
                "echo ${OGUN_HOME:-unset} ${ZEUS_STATE_DIR:-unset} $HOME".into(),
            ],
            &ExecOptions {
                timeout_ms: 10_000,
                ..ExecOptions::default()
            },
        )
        .await
        .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("unset unset "));
        let slow = exec_in(
            &record,
            &["sh".into(), "-c".into(), "sleep 5".into()],
            &ExecOptions {
                timeout_ms: 200,
                ..ExecOptions::default()
            },
        )
        .await
        .unwrap();
        assert!(slow.timed_out);
    }
}
