//! Agents under test and one agent run (`agr_`, O-06 section 7).
//!
//! An agent is a command a person configured: never text from a prompt or a task. It runs either inside the
//! environment (`container` placement, the default) or on the host (`host` placement) with the environment
//! reachable only through `ogun-bench exec`, so a host agent can call its model provider while the task
//! environment itself stays without network. Either way it gets a cleared environment: the variables below, the
//! non-secret `env` of its spec, and the model credentials named in `pass_env`, nothing else.
//!
//! | Variable | Value |
//! |---|---|
//! | `OGUN_BENCH_TASK` | Task id |
//! | `OGUN_BENCH_SEED` | Seed |
//! | `OGUN_BENCH_INSTRUCTION` | Path of `instruction.md` |
//! | `OGUN_BENCH_TRAJECTORY` | Path of the agent's trajectory log (`logs/agent/ogun-trajectory.jsonl`) |
//! | `OGUN_BENCH_EXEC_BIN`, `OGUN_BENCH_ENV_DIR` | Host placement: run `$OGUN_BENCH_EXEC_BIN exec --env-dir $OGUN_BENCH_ENV_DIR -- <argv>` |

use std::collections::BTreeMap;
use std::path::PathBuf;

use ogun_lineage::records::{Role, Stream};
use ogun_lineage::{AgentInfo, TrajectoryKind, TrajectoryWriter};
use ogun_types::Timestamp;
use serde::{Deserialize, Serialize};

use crate::env::{EnvRecord, Environment, ExecOptions, ExecOutput, Runtime, exec_in, expand};
use crate::task::Task;
use crate::{BenchError, BenchResult, jcs_blake3};

/// Most output lines recorded per stream as `agent.output` events.
pub const MAX_OUTPUT_LINES: usize = 2_000;
/// Most agent-reported lines ingested from the agent's own trajectory log.
pub const MAX_AGENT_LINES: usize = 100_000;

/// Where the agent process runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Inside the environment, through the runtime's `exec`.
    #[default]
    Container,
    /// On the host, with a cleared environment and `ogun-bench exec` to reach the environment.
    Host,
}

/// An agent configuration (part of a matrix).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// Name.
    pub name: String,
    /// Version.
    pub version: String,
    /// Model, when the agent takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Placement.
    #[serde(default)]
    pub placement: Placement,
    /// Command, with `{instruction_file}`, `{workspace}`, `{logs}`, `{benchmark}`, `{cache}`, `{seed}`,
    /// `{task_id}`, `{trajectory_file}` and `{model}` placeholders.
    pub command: Vec<String>,
    /// Model credentials passed by name from the harness's environment. The only secrets a run receives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pass_env: Vec<String>,
    /// Non-secret variables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Time limit; the task's own limit applies when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// The system prompt file, hashed into the record when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<PathBuf>,
    /// The tool-definition file, hashed into the record when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_file: Option<PathBuf>,
}

fn file_blake3(p: &Option<PathBuf>) -> Option<String> {
    p.as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| blake3::hash(&b).to_hex().to_string())
}

fn forbidden_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("OGUN_") || upper.starts_with("ZEUS_") || upper == "PATH" || upper == "HOME"
}

impl AgentSpec {
    /// Checks names and the command.
    ///
    /// # Errors
    ///
    /// [`BenchError::Spec`] for an empty command, or a variable that would reach Ogun's or Zeus's state.
    pub fn validate(&self) -> BenchResult<()> {
        if self.command.is_empty() {
            return Err(BenchError::Spec(format!(
                "agent {} has an empty command",
                self.name
            )));
        }
        if let Some(n) = self
            .pass_env
            .iter()
            .chain(self.env.keys())
            .find(|n| forbidden_name(n) || n.is_empty() || n.contains('='))
        {
            return Err(BenchError::Spec(format!(
                "agent {} may not set or pass {n}: benchmark runs never see OGUN_*, ZEUS_*, PATH or HOME",
                self.name
            )));
        }
        Ok(())
    }

    /// BLAKE3 of the configuration (credential names, never their values).
    #[must_use]
    pub fn config_blake3(&self) -> String {
        jcs_blake3(&serde_json::json!({
            "name": self.name,
            "version": self.version,
            "model": self.model,
            "placement": self.placement,
            "command": self.command,
            "pass_env": self.pass_env,
            "env": self.env,
            "timeout_ms": self.timeout_ms,
            "prompt_blake3": file_blake3(&self.prompt_file),
            "tools_blake3": file_blake3(&self.tools_file),
        }))
    }

    /// The agent-run record's view.
    #[must_use]
    pub fn info(&self) -> AgentInfo {
        AgentInfo {
            name: self.name.clone(),
            version: self.version.clone(),
            model: self.model.clone(),
            config_blake3: self.config_blake3(),
            prompt_blake3: file_blake3(&self.prompt_file),
            tools_blake3: file_blake3(&self.tools_file),
        }
    }

    /// The model credentials' values, for scrubbing the trajectory.
    #[must_use]
    pub fn secret_values(&self) -> Vec<zeroize::Zeroizing<String>> {
        self.pass_env
            .iter()
            .filter_map(|n| std::env::var(n).ok())
            .map(zeroize::Zeroizing::new)
            .collect()
    }
}

/// What the agent process did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProcess {
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Stopped at the limit.
    pub timed_out: bool,
    /// Wall time.
    pub agent_ms: u64,
}

fn output_events(w: &mut TrajectoryWriter, out: &ExecOutput) -> BenchResult<()> {
    for (stream, bytes) in [(Stream::Stdout, &out.stdout), (Stream::Stderr, &out.stderr)] {
        let text = String::from_utf8_lossy(bytes);
        for line in text.lines().take(MAX_OUTPUT_LINES) {
            w.append(
                Timestamp::now(),
                TrajectoryKind::AgentOutput {
                    stream,
                    untrusted_text: line.to_owned(),
                    truncated: false,
                },
            )?;
        }
    }
    Ok(())
}

/// Ingests the lines the agent wrote to its own trajectory log, refusing harness-only kinds. Returns the count.
///
/// # Errors
///
/// [`BenchError::Lineage`] when writing fails.
pub fn ingest_agent_log(w: &mut TrajectoryWriter, env: &Environment) -> BenchResult<u64> {
    let path = env.logs.join("agent").join("ogun-trajectory.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(0);
    };
    let mut n = 0;
    let mut refused = 0u64;
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(MAX_AGENT_LINES)
    {
        match ogun_lineage::records::parse_agent_line(line) {
            Some(kind) => {
                w.append(Timestamp::now(), kind)?;
                n += 1;
            }
            None => refused += 1,
        }
    }
    if refused > 0 {
        w.append(
            Timestamp::now(),
            TrajectoryKind::Message {
                role: Role::System,
                untrusted_text: format!(
                    "harness: {refused} agent log lines were not agent-reportable events"
                ),
                truncated: false,
            },
        )?;
    }
    Ok(n)
}

/// Runs the agent once in `env`, recording `agent.started`, its output and `agent.exited`.
///
/// # Errors
///
/// [`BenchError::Environment`] when the process cannot be started; [`BenchError::Lineage`] when recording fails.
pub async fn run_agent(
    spec: &AgentSpec,
    task: &Task,
    env: &Environment,
    seed: u64,
    w: &mut TrajectoryWriter,
) -> BenchResult<AgentProcess> {
    let paths = match spec.placement {
        Placement::Container => env.paths(),
        Placement::Host => env.host_paths(),
    };
    let trajectory_file = format!("{}/agent/ogun-trajectory.jsonl", paths.logs);
    let mut vars = BTreeMap::new();
    vars.insert("instruction_file", paths.instruction_file.clone());
    vars.insert("workspace", paths.workspace.clone());
    vars.insert("logs", paths.logs.clone());
    vars.insert("benchmark", paths.benchmark.clone());
    vars.insert("cache", paths.cache.clone());
    vars.insert("seed", seed.to_string());
    vars.insert("task_id", task.id.clone());
    vars.insert("trajectory_file", trajectory_file.clone());
    vars.insert("model", spec.model.clone().unwrap_or_default());
    let argv = expand(&spec.command, &vars);

    let mut env_vars = spec.env.clone();
    env_vars.insert("OGUN_BENCH_TASK".into(), task.id.clone());
    env_vars.insert("OGUN_BENCH_SEED".into(), seed.to_string());
    env_vars.insert(
        "OGUN_BENCH_INSTRUCTION".into(),
        paths.instruction_file.clone(),
    );
    env_vars.insert("OGUN_BENCH_TRAJECTORY".into(), trajectory_file);
    if spec.placement == Placement::Host {
        if let Ok(exe) = std::env::current_exe() {
            env_vars.insert("OGUN_BENCH_EXEC_BIN".into(), exe.display().to_string());
        }
        env_vars.insert("OGUN_BENCH_ENV_DIR".into(), env.dir.display().to_string());
    }
    let opts = ExecOptions {
        env: env_vars,
        pass_env: spec.pass_env.clone(),
        timeout_ms: spec.timeout_ms.unwrap_or(task.agent_timeout_ms),
    };
    w.append(
        Timestamp::now(),
        TrajectoryKind::AgentStarted {
            argv_blake3: jcs_blake3(&spec.command),
        },
    )?;
    let out = match spec.placement {
        Placement::Container => env.exec(&argv, &opts).await?,
        Placement::Host => {
            // The host agent needs the network for its model provider; the environment does not have it.
            let host = EnvRecord {
                id: env.id,
                runtime: Runtime::Local,
                container: None,
                workdir: env.workspace.display().to_string(),
                workspace: env.workspace.clone(),
                logs: env.logs.join("agent"),
                local_netns: false,
            };
            exec_in(&host, &argv, &opts).await?
        }
    };
    output_events(w, &out)?;
    w.append(
        Timestamp::now(),
        TrajectoryKind::AgentExited {
            exit_code: out.exit_code,
            agent_ms: out.duration_ms,
            timed_out: out.timed_out,
        },
    )?;
    Ok(AgentProcess {
        exit_code: out.exit_code,
        timed_out: out.timed_out,
        agent_ms: out.duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_cannot_reach_ogun_or_zeus_variables() {
        let mut a = AgentSpec {
            name: "a".into(),
            version: "1".into(),
            model: None,
            placement: Placement::Container,
            command: vec!["run".into()],
            pass_env: vec!["ANTHROPIC_API_KEY".into()],
            env: BTreeMap::new(),
            timeout_ms: None,
            prompt_file: None,
            tools_file: None,
        };
        a.validate().unwrap();
        let before = a.config_blake3();
        a.pass_env.push("ZEUS_STATE_DIR".into());
        assert!(a.validate().is_err());
        assert_ne!(before, a.config_blake3());
    }
}
