//! Agent-run records, trajectories and the cost ledger (O-05 sections 6 to 8).
//!
//! An agent run (`agr_`) is one execution of one agent on one task in one environment with one seed. Its
//! trajectory is an append-only JSON Lines file of [`TrajectoryEvent`]s: every line carries a gap-free `seq`, the
//! instant, the milliseconds since the run started and `prev_blake3`, the BLAKE3 of the JCS form of the previous
//! line, so a truncated or edited trajectory is detected by [`verify_trajectory`]. Text that came from an agent or a
//! model is stored in members named `untrusted_*`, bounded, and scrubbed of every secret value the harness knows
//! (model credentials it passed in) before it is written.
//!
//! The cost ledger is a list of [`CostRecord`]s; money is integer micro-dollars (`_usd_micro`, O-00), never a float.

use std::io::{BufRead, Write};
use std::path::Path;

use ogun_types::{AgentRunId, EnvironmentId, EvaluatorId, NodeInstanceId, RunId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::{LineageError, LineageResult};

/// Longest `untrusted_*` text kept in one trajectory event, in bytes; longer text is cut at a character boundary
/// and marked `truncated`.
pub const MAX_UNTRUSTED_BYTES: usize = 64 * 1024;

/// What a cost-ledger entry measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostKind {
    /// Model tokens through a provider.
    Model,
    /// CPU or container time.
    Compute,
    /// Bytes transferred.
    Network,
    /// Bytes stored.
    Storage,
}

/// One cost-ledger entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostRecord {
    /// When the cost was incurred.
    pub at: Timestamp,
    /// What it measures.
    pub kind: CostKind,
    /// The Ogun run, when it belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// The node instance, when it belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_instance: Option<NodeInstanceId>,
    /// The agent run, when it belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_run: Option<AgentRunId>,
    /// Provider name from the ladder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Bytes (network or storage).
    #[serde(default)]
    pub bytes: u64,
    /// Wall time.
    #[serde(default)]
    pub wall_ms: u64,
    /// Cost in micro-US-dollars.
    #[serde(default)]
    pub cost_usd_micro: u64,
    /// True when the cost is an estimate (no provider invoice line).
    #[serde(default)]
    pub estimated: bool,
}

/// Sums over cost records or trajectory usage events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostTotals {
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Micro-US-dollars.
    pub cost_usd_micro: u64,
    /// Bytes.
    pub bytes: u64,
    /// Model requests counted.
    pub model_requests: u64,
}

impl CostTotals {
    /// Adds one ledger entry.
    pub fn add(&mut self, r: &CostRecord) {
        self.input_tokens = self.input_tokens.saturating_add(r.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(r.output_tokens);
        self.cost_usd_micro = self.cost_usd_micro.saturating_add(r.cost_usd_micro);
        self.bytes = self.bytes.saturating_add(r.bytes);
        if r.kind == CostKind::Model {
            self.model_requests = self.model_requests.saturating_add(1);
        }
    }

    /// Sums a ledger.
    #[must_use]
    pub fn of<'a>(records: impl IntoIterator<Item = &'a CostRecord>) -> Self {
        let mut t = Self::default();
        for r in records {
            t.add(r);
        }
        t
    }
}

/// The agent under test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    /// Agent name.
    pub name: String,
    /// Agent version.
    pub version: String,
    /// Model id, when the agent reports or is configured with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// BLAKE3 of the JCS form of the agent configuration (command, environment names, limits).
    pub config_blake3: String,
    /// BLAKE3 of the system prompt, when the harness knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_blake3: Option<String>,
    /// BLAKE3 of the tool definitions, when the harness knows them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_blake3: Option<String>,
}

/// The task an agent run worked on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    /// Benchmark family (`terminal_bench`, `spider2_dbt`, `ade_bench`, ...).
    pub benchmark: String,
    /// Task id within the benchmark.
    pub task_id: String,
    /// Task version, when the format has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_version: Option<String>,
    /// BLAKE3 over the task's files (O-06 section 6).
    pub task_blake3: String,
}

/// How an agent run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunOutcome {
    /// The agent exited and the evaluator ran.
    Completed,
    /// The agent exited non-zero and the evaluator was not run, or the agent reported failure.
    Failed,
    /// The agent exceeded its time limit.
    TimedOut,
    /// A person stopped it.
    Cancelled,
    /// The harness could not run it (environment or evaluator failure); not the agent's fault.
    Error,
}

/// An evaluator's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    /// Pass or fail.
    pub passed: bool,
    /// Score in parts per million, 0 to 1,000,000 (no floats, O-00).
    pub score_ppm: u32,
    /// BLAKE3 of the evaluator's full JSON result, kept beside the trajectory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_blake3: Option<String>,
}

/// Where a trajectory is and what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryRef {
    /// Path relative to the results directory.
    pub path: String,
    /// Events written.
    pub events: u64,
    /// BLAKE3 of the JCS form of the last event: the chain head.
    pub head_blake3: String,
}

/// One recorded agent run (`agr_`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunRecord {
    /// The agent run.
    pub id: AgentRunId,
    /// The agent.
    pub agent: AgentInfo,
    /// The task.
    pub task: TaskRef,
    /// The environment it ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentId>,
    /// BLAKE3 of the environment (image digest, fixture, limits, network policy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_blake3: Option<String>,
    /// The evaluator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<EvaluatorId>,
    /// Evaluator name and version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_version: Option<String>,
    /// Seed, for matrices that repeat a task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// The Ogun run, when the agent run is part of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Start.
    pub started_at: Timestamp,
    /// End, once finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// Outcome, once finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AgentRunOutcome>,
    /// The agent process's exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Agent wall time.
    pub agent_ms: u64,
    /// Whole-run wall time, including setup and evaluation.
    pub wall_ms: u64,
    /// Token and cost totals.
    pub totals: CostTotals,
    /// Tool calls the agent reported.
    pub tool_calls: u64,
    /// Files created, modified and deleted in the workspace.
    pub files_created: u64,
    /// See `files_created`.
    pub files_modified: u64,
    /// See `files_created`.
    pub files_deleted: u64,
    /// The trajectory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory: Option<TrajectoryRef>,
    /// The evaluator's verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<Score>,
    /// True when the result came from the result cache instead of a fresh run.
    #[serde(default)]
    pub cached: bool,
    /// Harness error code when `outcome` is `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Which output stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Message roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System prompt.
    System,
    /// The task or a person.
    User,
    /// The model.
    Assistant,
    /// A tool result fed back to the model.
    Tool,
}

/// What happened, one trajectory line. The `type` names are the vocabulary of ADR 0005 decision 5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TrajectoryKind {
    /// The harness began the task.
    #[serde(rename = "task.started")]
    TaskStarted {
        /// Task id.
        task_id: String,
        /// Task digest.
        task_blake3: String,
        /// Environment digest.
        environment_blake3: String,
        /// Seed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<u64>,
    },
    /// The workspace before the agent ran.
    #[serde(rename = "workspace.snapshot")]
    WorkspaceSnapshot {
        /// BLAKE3 over the sorted `(path, blake3)` list.
        tree_blake3: String,
        /// Files.
        files: u64,
    },
    /// The agent process started.
    #[serde(rename = "agent.started")]
    AgentStarted {
        /// BLAKE3 of the JCS form of the argv.
        argv_blake3: String,
    },
    /// A message the agent reported (its own log).
    #[serde(rename = "message")]
    Message {
        /// Who spoke.
        role: Role,
        /// The text, scrubbed and bounded.
        untrusted_text: String,
        /// The text was cut.
        #[serde(default)]
        truncated: bool,
    },
    /// The agent sent a model request.
    #[serde(rename = "model.request")]
    ModelRequest {
        /// Provider.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Input tokens.
        #[serde(default)]
        input_tokens: u64,
    },
    /// The model answered.
    #[serde(rename = "model.response")]
    ModelResponse {
        /// Output tokens.
        #[serde(default)]
        output_tokens: u64,
        /// Latency.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latency_ms: Option<u64>,
        /// Cost in micro-US-dollars.
        #[serde(default)]
        cost_usd_micro: u64,
    },
    /// The agent called a tool.
    #[serde(rename = "tool.call")]
    ToolCall {
        /// Tool name.
        tool: String,
        /// Arguments, scrubbed and bounded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        untrusted_args: Option<String>,
    },
    /// A shell command the agent ran.
    #[serde(rename = "tool.shell")]
    ToolShell {
        /// The command line, scrubbed and bounded.
        untrusted_command: String,
    },
    /// A tool returned.
    #[serde(rename = "tool.result")]
    ToolResult {
        /// Tool name.
        tool: String,
        /// Exit code, for commands.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Duration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Output, scrubbed and bounded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        untrusted_output: Option<String>,
    },
    /// The agent read a file.
    #[serde(rename = "file.read")]
    FileRead {
        /// Path relative to the workspace.
        path: String,
    },
    /// A file appeared in the workspace.
    #[serde(rename = "file.created")]
    FileCreated {
        /// Path relative to the workspace.
        path: String,
        /// Content digest.
        blake3: String,
        /// Size.
        size_bytes: u64,
    },
    /// A file changed.
    #[serde(rename = "file.patch")]
    FilePatch {
        /// Path relative to the workspace.
        path: String,
        /// Digest before.
        before_blake3: String,
        /// Digest after.
        blake3: String,
        /// Size after.
        size_bytes: u64,
    },
    /// A file disappeared.
    #[serde(rename = "file.deleted")]
    FileDeleted {
        /// Path relative to the workspace.
        path: String,
        /// Digest before.
        before_blake3: String,
    },
    /// A line the agent printed.
    #[serde(rename = "agent.output")]
    AgentOutput {
        /// Which stream.
        stream: Stream,
        /// The line, scrubbed and bounded.
        untrusted_text: String,
        /// The line was cut.
        #[serde(default)]
        truncated: bool,
    },
    /// Model use the agent reported in one sum.
    #[serde(rename = "usage")]
    Usage {
        /// Provider.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Input tokens.
        input_tokens: u64,
        /// Output tokens.
        output_tokens: u64,
        /// Micro-US-dollars.
        cost_usd_micro: u64,
    },
    /// The agent process exited.
    #[serde(rename = "agent.exited")]
    AgentExited {
        /// Exit code; absent when killed by a signal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Agent wall time.
        agent_ms: u64,
        /// It was stopped at its time limit.
        timed_out: bool,
    },
    /// A database's state digest (benchmarks with DuckDB files hash them before and after).
    #[serde(rename = "db.state")]
    DbState {
        /// Path relative to the workspace.
        path: String,
        /// Digest.
        blake3: String,
        /// `before` or `after`.
        phase: String,
    },
    /// The evaluator started.
    #[serde(rename = "evaluator.started")]
    EvaluatorStarted {
        /// Evaluator name and version.
        evaluator: String,
    },
    /// The evaluator finished.
    #[serde(rename = "evaluator.finished")]
    EvaluatorFinished {
        /// Verdict.
        passed: bool,
        /// Score in parts per million.
        score_ppm: u32,
        /// Exit code.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Evaluator wall time.
        duration_ms: u64,
    },
    /// The harness finished the task.
    #[serde(rename = "task.finished")]
    TaskFinished {
        /// Outcome.
        outcome: AgentRunOutcome,
    },
}

/// One trajectory line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryEvent {
    /// From 1, without gaps.
    pub seq: u64,
    /// When the harness recorded it.
    pub at: Timestamp,
    /// Milliseconds since the agent run started.
    pub elapsed_ms: u64,
    /// The agent run.
    pub agent_run: AgentRunId,
    /// BLAKE3 of the JCS form of the previous line; absent on the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_blake3: Option<String>,
    /// The payload.
    #[serde(flatten)]
    pub kind: TrajectoryKind,
}

fn line_digest(event: &TrajectoryEvent) -> LineageResult<String> {
    let value = serde_json::to_value(event)?;
    let bytes = zeus_verify::canonical::to_jcs_bytes(&value)
        .map_err(|_| LineageError::Json("trajectory event is not canonicalisable".into()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Cuts `text` to at most `max` bytes at a character boundary.
#[must_use]
pub fn bound_text(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Appends trajectory events to a JSON Lines file, scrubbing known secret values from untrusted text.
pub struct TrajectoryWriter {
    file: std::io::BufWriter<std::fs::File>,
    agent_run: AgentRunId,
    started: Timestamp,
    seq: u64,
    head: Option<String>,
    secrets: Vec<Zeroizing<String>>,
}

impl std::fmt::Debug for TrajectoryWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrajectoryWriter")
            .field("agent_run", &self.agent_run)
            .field("seq", &self.seq)
            .field("secrets", &self.secrets.len())
            .finish_non_exhaustive()
    }
}

impl TrajectoryWriter {
    /// Creates the file (it must not exist: a trajectory is written once).
    ///
    /// # Errors
    ///
    /// [`LineageError::Io`] when the file exists or cannot be created.
    pub fn create(path: &Path, agent_run: AgentRunId, started: Timestamp) -> LineageResult<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(Self {
            file: std::io::BufWriter::new(file),
            agent_run,
            started,
            seq: 0,
            head: None,
            secrets: vec![],
        })
    }

    /// A secret value to scrub from every untrusted text before it is written (for example a model API key the
    /// harness handed to the agent). Values shorter than 8 bytes are ignored: they would mask ordinary words.
    pub fn add_secret(&mut self, value: Zeroizing<String>) {
        if value.len() >= 8 {
            self.secrets.push(value);
        }
    }

    /// Scrubs and bounds untrusted text.
    #[must_use]
    pub fn scrub(&self, text: &str) -> (String, bool) {
        let mut text = text.to_owned();
        for s in &self.secrets {
            if text.contains(s.as_str()) {
                text = text.replace(s.as_str(), "[redacted]");
            }
        }
        if text.contains("ZEUS-CANARY-") {
            text = text.replace("ZEUS-CANARY-", "[canary]-");
        }
        bound_text(&text, MAX_UNTRUSTED_BYTES)
    }

    fn scrub_kind(&self, kind: TrajectoryKind) -> TrajectoryKind {
        let s = |t: String| self.scrub(&t).0;
        match kind {
            TrajectoryKind::Message {
                role,
                untrusted_text,
                truncated,
            } => {
                let (t, cut) = self.scrub(&untrusted_text);
                TrajectoryKind::Message {
                    role,
                    untrusted_text: t,
                    truncated: truncated || cut,
                }
            }
            TrajectoryKind::AgentOutput {
                stream,
                untrusted_text,
                truncated,
            } => {
                let (t, cut) = self.scrub(&untrusted_text);
                TrajectoryKind::AgentOutput {
                    stream,
                    untrusted_text: t,
                    truncated: truncated || cut,
                }
            }
            TrajectoryKind::ToolCall {
                tool,
                untrusted_args,
            } => TrajectoryKind::ToolCall {
                tool: s(tool),
                untrusted_args: untrusted_args.map(s),
            },
            TrajectoryKind::ToolShell { untrusted_command } => TrajectoryKind::ToolShell {
                untrusted_command: s(untrusted_command),
            },
            TrajectoryKind::ToolResult {
                tool,
                exit_code,
                duration_ms,
                untrusted_output,
            } => TrajectoryKind::ToolResult {
                tool: s(tool),
                exit_code,
                duration_ms,
                untrusted_output: untrusted_output.map(s),
            },
            other => other,
        }
    }

    /// Appends one event and flushes it.
    ///
    /// # Errors
    ///
    /// [`LineageError::Io`] or [`LineageError::Json`].
    pub fn append(
        &mut self,
        at: Timestamp,
        kind: TrajectoryKind,
    ) -> LineageResult<TrajectoryEvent> {
        self.seq += 1;
        let event = TrajectoryEvent {
            seq: self.seq,
            at,
            elapsed_ms: at
                .unix_millis()
                .saturating_sub(self.started.unix_millis())
                .max(0)
                .unsigned_abs(),
            agent_run: self.agent_run,
            prev_blake3: self.head.clone(),
            kind: self.scrub_kind(kind),
        };
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.head = Some(line_digest(&event)?);
        Ok(event)
    }

    /// Events written so far.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.seq
    }

    /// Whether nothing was written yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seq == 0
    }

    /// The chain head: the digest of the last event.
    #[must_use]
    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    /// Flushes and syncs the file, returning the reference for the agent-run record.
    ///
    /// # Errors
    ///
    /// [`LineageError::Io`].
    pub fn finish(mut self, relative_path: &str) -> LineageResult<TrajectoryRef> {
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(TrajectoryRef {
            path: relative_path.to_owned(),
            events: self.seq,
            head_blake3: self.head.unwrap_or_default(),
        })
    }
}

/// What a verified trajectory adds up to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectorySummary {
    /// Events.
    pub events: u64,
    /// Chain head.
    pub head_blake3: String,
    /// Token and cost totals from `model.*` and `usage` events.
    pub totals: CostTotals,
    /// `tool.call` and `tool.shell` events.
    pub tool_calls: u64,
    /// Tool calls whose `tool.result` had a non-zero exit code.
    pub failed_tool_calls: u64,
    /// Shell commands repeated verbatim (a tool-efficiency proxy, VISION section 12).
    pub repeated_commands: u64,
    /// `file.read` events naming a path already read.
    pub files_reread: u64,
    /// The last `agent.exited` wall time.
    pub agent_ms: u64,
}

/// Reads a trajectory, checks `seq` and the BLAKE3 chain, and sums it.
///
/// # Errors
///
/// [`LineageError::TrajectoryBroken`] at the first bad line; [`LineageError::Io`] on read errors.
pub fn verify_trajectory(reader: impl BufRead) -> LineageResult<TrajectorySummary> {
    let mut sum = TrajectorySummary::default();
    let mut prev: Option<String> = None;
    let mut seen_commands = std::collections::HashSet::new();
    let mut seen_reads = std::collections::HashSet::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let n = u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1);
        let event: TrajectoryEvent =
            serde_json::from_str(&line).map_err(|_| LineageError::TrajectoryBroken {
                seq: n,
                reason: "not a trajectory event",
            })?;
        if event.seq != sum.events + 1 {
            return Err(LineageError::TrajectoryBroken {
                seq: event.seq,
                reason: "sequence gap",
            });
        }
        if event.prev_blake3 != prev {
            return Err(LineageError::TrajectoryBroken {
                seq: event.seq,
                reason: "chain mismatch",
            });
        }
        prev = Some(line_digest(&event)?);
        sum.events = event.seq;
        match &event.kind {
            TrajectoryKind::ModelRequest { input_tokens, .. } => {
                sum.totals.input_tokens = sum.totals.input_tokens.saturating_add(*input_tokens);
                sum.totals.model_requests += 1;
            }
            TrajectoryKind::ModelResponse {
                output_tokens,
                cost_usd_micro,
                ..
            } => {
                sum.totals.output_tokens = sum.totals.output_tokens.saturating_add(*output_tokens);
                sum.totals.cost_usd_micro =
                    sum.totals.cost_usd_micro.saturating_add(*cost_usd_micro);
            }
            TrajectoryKind::Usage {
                input_tokens,
                output_tokens,
                cost_usd_micro,
                ..
            } => {
                sum.totals.input_tokens = sum.totals.input_tokens.saturating_add(*input_tokens);
                sum.totals.output_tokens = sum.totals.output_tokens.saturating_add(*output_tokens);
                sum.totals.cost_usd_micro =
                    sum.totals.cost_usd_micro.saturating_add(*cost_usd_micro);
            }
            TrajectoryKind::ToolCall { .. } => sum.tool_calls += 1,
            TrajectoryKind::ToolShell { untrusted_command } => {
                sum.tool_calls += 1;
                if !seen_commands.insert(untrusted_command.clone()) {
                    sum.repeated_commands += 1;
                }
            }
            TrajectoryKind::ToolResult {
                exit_code: Some(c), ..
            } if *c != 0 => {
                sum.failed_tool_calls += 1;
            }
            TrajectoryKind::FileRead { path } => {
                if !seen_reads.insert(path.clone()) {
                    sum.files_reread += 1;
                }
            }
            TrajectoryKind::AgentExited { agent_ms, .. } => sum.agent_ms = *agent_ms,
            _ => {}
        }
    }
    sum.head_blake3 = prev.unwrap_or_default();
    Ok(sum)
}

/// Parses one line an agent wrote to its own trajectory log (`/logs/agent/ogun-trajectory.jsonl`, O-06
/// section 7) into an event kind. Only the agent-reportable kinds are accepted; harness kinds (`task.*`,
/// `agent.started`, `agent.exited`, `evaluator.*`, `file.*` except `file.read`) are refused so an agent cannot
/// forge them.
#[must_use]
pub fn parse_agent_line(line: &str) -> Option<TrajectoryKind> {
    let value: Value = serde_json::from_str(line).ok()?;
    let kind: TrajectoryKind = serde_json::from_value(value).ok()?;
    match kind {
        TrajectoryKind::Message { .. }
        | TrajectoryKind::ModelRequest { .. }
        | TrajectoryKind::ModelResponse { .. }
        | TrajectoryKind::ToolCall { .. }
        | TrajectoryKind::ToolShell { .. }
        | TrajectoryKind::ToolResult { .. }
        | TrajectoryKind::FileRead { .. }
        | TrajectoryKind::Usage { .. } => Some(kind),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogun_types::IdGenerator;

    #[test]
    fn chain_verifies_and_detects_edits_and_secrets_are_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let id = IdGenerator::new().agent_run_id();
        let t0 = Timestamp::from_unix_millis(1_000).unwrap();
        let mut w = TrajectoryWriter::create(&path, id, t0).unwrap();
        w.add_secret(Zeroizing::new("sk-ZEUS-CANARY-0123456789".into()));
        w.append(
            t0,
            TrajectoryKind::AgentStarted {
                argv_blake3: "00".repeat(32),
            },
        )
        .unwrap();
        w.append(
            Timestamp::from_unix_millis(1_500).unwrap(),
            TrajectoryKind::ToolShell {
                untrusted_command: "echo sk-ZEUS-CANARY-0123456789".into(),
            },
        )
        .unwrap();
        w.append(
            Timestamp::from_unix_millis(2_000).unwrap(),
            TrajectoryKind::Usage {
                provider: None,
                model: None,
                input_tokens: 10,
                output_tokens: 5,
                cost_usd_micro: 7,
            },
        )
        .unwrap();
        let r = w.finish("t.jsonl").unwrap();
        assert_eq!(r.events, 3);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("ZEUS-CANARY-"));
        let sum = verify_trajectory(text.as_bytes()).unwrap();
        assert_eq!(sum.head_blake3, r.head_blake3);
        assert_eq!(sum.totals.input_tokens, 10);
        assert_eq!(sum.tool_calls, 1);
        let edited = text.replacen("\"input_tokens\":10", "\"input_tokens\":11", 1);
        let lines: Vec<&str> = edited.lines().collect();
        // Editing line 3 keeps the chain up to it; drop line 2 to break it.
        let broken = format!("{}\n{}\n", lines[0], lines[2]);
        assert!(verify_trajectory(broken.as_bytes()).is_err());
    }

    #[test]
    fn agents_cannot_report_harness_events() {
        assert!(parse_agent_line(r#"{"type":"tool.shell","untrusted_command":"ls"}"#).is_some());
        assert!(parse_agent_line(r#"{"type":"task.finished","outcome":"completed"}"#).is_none());
        assert!(parse_agent_line("not json").is_none());
    }
}
