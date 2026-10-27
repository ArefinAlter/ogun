//! The child-process runner: protocol `jsonl-stdio/0` (O-03 section 4).
//!
//! 1. The command starts in the sandbox ([`crate::sandbox`]) with a clean environment, its working
//!    directory set to the attempt's fresh scratch directory, and only stdin, stdout and stderr.
//! 2. The runner writes one `start` line to stdin and closes it:
//!    `{"type": "start", "protocol": "jsonl-stdio/0", "op", "run_id", "node_instance", "node_id",
//!    "partition", "window", "attempt", "deadline_ms", "run_start_ms", "scratch", "workspace", "config", "inputs": [{"port",
//!    "ref", "path", "blake3", "media_type", "size_bytes", "metadata"}], "code": {"path", "dir", "blake3"},
//!    "checkpoint", "secrets": {...}}`. `config` keeps its `{"secret_ref": ...}` objects; `secrets` maps
//!    exactly the names the node uses to their values, and appears nowhere else. An input's `port` is the
//!    producing node's id (`sales` for `sales.tables`); `ref` is the full reference.
//! 3. The child writes JSON lines to stdout: `progress` (`done`, `total`), `output` (`port`, `path`
//!    relative to scratch or absolute inside it, `media_type`, `metadata`), `checkpoint` (`checkpoint`,
//!    at most 64 KiB), `usage` (`provider`, `model`, `input_tokens`, `output_tokens`, `cost_usd_micro`;
//!    model operators, O-03 section 8.3), `log` (`message`), and finally exactly one `result` (`ok: true`) or `error` (an
//!    envelope, O-00 section 5, either top level or under `error`).
//! 4. stderr is captured as diagnostics: at most 64 KiB per attempt, lines cut at 2 KiB, every resolved
//!    secret value and every URL query removed, then sent as [`OpEvent::Log`].
//! 5. Exceeding the invocation's `timeout_ms` is `op.timeout`; [`Context::cancel`] kills the process tree and
//!    the attempt ends `Cancelled`. An exit without `result` or `error` is `op.crashed`; a CPU, file size or
//!    system call limit hit is `op.sandbox_violation`, as is an output on an undeclared port or outside
//!    scratch.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::error::{OpError, OpResult, internal};
use crate::manifest::{CodeBundle, LoadedManifest, ManifestKind, NetworkGrant};
use crate::metadata::integerize;
use crate::sandbox::{self, Backend, Plan};
use crate::secrets::{ResolvedSecrets, Scrubber};
use crate::{Context, Environment, Invocation, OpEvent, Outcome, OutputFile, files, tools};

/// Longest stdout line accepted.
const MAX_LINE: usize = 1024 * 1024;
/// Largest checkpoint.
const MAX_CHECKPOINT: usize = 64 * 1024;
/// stderr kept per attempt.
const MAX_STDERR: usize = 64 * 1024;
/// One stderr line.
const MAX_STDERR_LINE: usize = 2048;
/// How long a child may linger after its `result` line.
const EXIT_GRACE: Duration = Duration::from_secs(10);

/// Everything one child attempt needs.
pub(crate) struct ChildRun<'a> {
    pub env: &'a Environment,
    pub manifest: &'a LoadedManifest,
    pub inv: &'a Invocation,
    pub config: &'a Value,
    pub secrets: &'a ResolvedSecrets,
    pub ctx: &'a Context,
}

fn send(ctx: &Context, ev: OpEvent) {
    let _ = ctx.events.send(ev);
}

/// Runs one attempt of a child operator.
pub(crate) async fn run(run: ChildRun<'_>) -> Outcome {
    if *run.ctx.cancel.borrow() {
        return Outcome::Cancelled;
    }
    match run_inner(&run).await {
        Ok(outcome) => outcome,
        Err(e) => e.into_outcome(),
    }
}

/// Run the compiled model library on the host, without executing a manifest command.
/// Subscription CLIs receive only the bounded inference prompt in their own empty scratch directory.
pub(crate) async fn run_native(run: ChildRun<'_>) -> Outcome {
    let inv = run.inv;
    let start = json!({"type":"start", "op":inv.op, "node_instance":inv.node_instance, "config":run.config, "inputs":inv.inputs, "secrets":{}});
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let model = ogun_op_llm::run_native(start, inv.scratch.clone(), tx);
    tokio::pin!(model);
    let deadline = tokio::time::sleep(Duration::from_millis(inv.timeout_ms.max(1)));
    tokio::pin!(deadline);
    let mut cancel = run.ctx.cancel.clone();
    let cancelled = async move {
        loop {
            if *cancel.borrow() {
                return;
            }
            if cancel.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::pin!(cancelled);
    let mut state = Collected::default();
    let scrubber = run.secrets.scrubber();
    let process = |value: Value, state: &mut Collected| -> OpResult<()> {
        let line =
            serde_json::to_vec(&value).map_err(|_| internal("native model protocol encoding"))?;
        if line.len() > MAX_LINE {
            return Err(OpError::new(
                "op.limit_exceeded",
                "Native model event exceeds its byte limit.",
            ));
        }
        handle_line(&line, &run, &scrubber, state)
    };
    loop {
        tokio::select! {
            biased;
            () = &mut cancelled => return Outcome::Cancelled,
            () = &mut deadline => return OpError::new("op.timeout","The native model attempt exceeded its deadline.").into_outcome(),
            value=rx.recv() => if let Some(value)=value && let Err(e)=process(value,&mut state) { return e.into_outcome(); },
            _ = &mut model => {
                while let Ok(value)=rx.try_recv() { if let Err(e)=process(value,&mut state) { return e.into_outcome(); } }
                return finish(&run,state,None).unwrap_or_else(OpError::into_outcome);
            }
        }
    }
}

struct Prepared {
    plan: Plan,
    backend: Backend,
    start_line: Zeroizing<Vec<u8>>,
}

async fn prepare(run: &ChildRun<'_>) -> OpResult<Prepared> {
    let m = &run.manifest.manifest;
    let inv = run.inv;
    if m.kind == ManifestKind::Model && !run.env.model_access {
        // The registry refuses earlier; this keeps the runner safe on its own (O-03 section 8.2 item 1).
        return Err(crate::registry::llm_not_configured(&format!(
            "{} calls a model and model access is off",
            m.name
        )));
    }
    let network = match &m.sandbox.network {
        NetworkGrant::None => false,
        NetworkGrant::Any => {
            if !run.env.network_access {
                return Err(OpError::new(
                    "op.network_denied",
                    format!(
                        "{} needs network access, which this environment denies",
                        m.name
                    ),
                ));
            }
            true
        }
        NetworkGrant::Allowlist(_) => {
            return Err(OpError::new(
                "op.sandbox_unavailable",
                "a network host allow-list cannot be enforced by this build's sandbox",
            ));
        }
    };
    let backend = sandbox::detect().await;
    if backend == Backend::None {
        if !run.env.allow_unsandboxed {
            return Err(OpError::new(
                "op.sandbox_unavailable",
                format!(
                    "no OS sandbox is available on this host for {}; child operators are refused",
                    m.name
                ),
            ));
        }
        tracing::warn!(op = %m.name, node_instance = %inv.node_instance, "running a child operator without an OS sandbox (allow_unsandboxed)");
        send(
            run.ctx,
            OpEvent::Log(format!(
                "warning: {} runs without an OS sandbox (development profile)",
                m.name
            )),
        );
    }

    // The command and the tools it drives.
    let mut search: Vec<PathBuf> = Vec::new();
    if let Some(dir) = run.manifest.dir() {
        search.push(dir.to_path_buf());
    }
    search.extend(run.env.operator_dirs.iter().cloned());
    let program_name = &m.invocation.command[0];
    let program = if let Some(dir) = run.manifest.dir()
        && program_name.contains('/')
    {
        tools::find(&dir.join(program_name).to_string_lossy(), &[])
    } else {
        tools::find(program_name, &search)
    }
    .ok_or_else(|| {
        OpError::new(
            "op.tool_missing",
            format!(
                "the command of {} ({program_name}) is not installed",
                m.name
            ),
        )
    })?;
    let mut tool_dirs: Vec<PathBuf> = Vec::new();
    for tool in &m.tools {
        let names: Vec<&str> = tool.any_of.iter().map(String::as_str).collect();
        match tools::find_any(&names, &search) {
            Some(path) => {
                let real = path.canonicalize().unwrap_or(path.clone());
                for p in [path, real] {
                    if let Some(dir) = p.parent()
                        && !tool_dirs.iter().any(|d| d == dir)
                    {
                        tool_dirs.push(dir.to_path_buf());
                    }
                }
            }
            None if tool.optional => {}
            None => {
                return Err(OpError::new(
                    "op.tool_missing",
                    format!(
                        "{} needs {}, which is not installed on this host",
                        m.name,
                        tool.any_of.join(" or ")
                    ),
                ));
            }
        }
    }

    // Code.
    let mut code_dir: Option<PathBuf> = None;
    if m.kind == ManifestKind::Code {
        let code = inv.code.as_ref().ok_or_else(|| {
            OpError::new(
                "op.config_invalid",
                format!(
                    "{} runs user code and the invocation has no verified code file",
                    m.name
                ),
            )
        })?;
        if m.code.is_some_and(|c| c.bundle == CodeBundle::Archive) {
            let dir = inv.scratch.join("code");
            crate::ops::archive::extract_file(
                &code.path,
                &dir,
                &crate::ops::archive::Limits::default(),
            )
            .map_err(|e| {
                OpError::new(
                    "op.input_invalid",
                    format!(
                        "the code bundle of {} cannot be extracted: {}",
                        m.name, e.error.message
                    ),
                )
            })?;
            code_dir = Some(dir);
        }
    }

    let substitute = |arg: &str| -> String {
        let mut a = arg.to_owned();
        if let Some(code) = &inv.code {
            a = a.replace("{code}", &code.path.to_string_lossy());
        }
        if let Some(dir) = &code_dir {
            a = a.replace("{code_dir}", &dir.to_string_lossy());
        }
        if let Some(dir) = run.manifest.dir() {
            a = a.replace("{manifest_dir}", &dir.to_string_lossy());
        }
        a.replace("{scratch}", &inv.scratch.to_string_lossy())
    };
    let args = m.invocation.command[1..]
        .iter()
        .map(|a| substitute(a).into())
        .collect();

    // What the sandbox exposes.
    let mut read_only: Vec<PathBuf> = Vec::new();
    if m.sandbox.grants("inputs:ro") {
        read_only.extend(inv.inputs.iter().map(|i| i.path.clone()));
    }
    if let Some(code) = &inv.code
        && (m.sandbox.grants("code:ro") || m.kind == ManifestKind::Code)
    {
        read_only.push(code.path.clone());
    }
    let workspace = if m.sandbox.grants("workspace:ro") {
        run.env.workspace.clone()
    } else {
        None
    };
    if let Some(ws) = &workspace {
        read_only.push(ws.clone());
    }
    let real_program = program.canonicalize().unwrap_or(program.clone());
    for p in [&program, &real_program] {
        if let Some(dir) = p.parent() {
            read_only.push(dir.to_path_buf());
        }
    }
    read_only.extend(tool_dirs.iter().cloned());

    let mut path = String::new();
    for dir in tool_dirs
        .iter()
        .map(PathBuf::as_path)
        .chain(program.parent())
    {
        path.push_str(&dir.to_string_lossy());
        path.push(if cfg!(windows) { ';' } else { ':' });
    }
    path.push_str(tools::CHILD_PATH);
    let tmp = if matches!(backend, Backend::Bwrap(_)) {
        "/tmp".to_owned()
    } else {
        let t = inv.scratch.join(".tmp");
        files::ensure_dir(&t)?;
        t.to_string_lossy().into_owned()
    };
    let scratch = inv.scratch.to_string_lossy().into_owned();
    let mut env: Vec<(String, String)> = vec![
        ("PATH".into(), path),
        ("HOME".into(), scratch.clone()),
        ("TMPDIR".into(), tmp.clone()),
        ("LANG".into(), "C.UTF-8".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("NO_COLOR".into(), "1".into()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("PYTHONUNBUFFERED".into(), "1".into()),
        ("OGUN_PROTOCOL".into(), crate::manifest::PROTOCOL.into()),
        ("OGUN_SCRATCH".into(), scratch),
    ];
    if cfg!(windows) {
        for key in ["SystemRoot", "SYSTEMROOT", "windir"] {
            if let Ok(v) = std::env::var(key) {
                env.push((key.into(), v));
            }
        }
        env.push(("TEMP".into(), tmp.clone()));
        env.push(("TMP".into(), tmp));
    }

    let start_line = start_line(run, code_dir.as_deref(), workspace.as_deref())?;
    Ok(Prepared {
        plan: Plan {
            program,
            args,
            scratch: inv.scratch.clone(),
            read_only,
            network,
            memory_bytes: m.sandbox.memory_bytes,
            cpu_ms: m.sandbox.cpu_ms,
            env,
        },
        backend,
        start_line,
    })
}

/// The `start` line. Built so that the secret values exist only in one zeroizing buffer of fixed capacity
/// (no reallocation leaves copies behind).
fn start_line(
    run: &ChildRun<'_>,
    code_dir: Option<&Path>,
    workspace: Option<&Path>,
) -> OpResult<Zeroizing<Vec<u8>>> {
    let inv = run.inv;
    let inputs: Vec<Value> = inv
        .inputs
        .iter()
        .map(|i| {
            let node = i.port.split_once('.').map_or(i.port.as_str(), |(n, _)| n);
            let mut o = json!({
                "port": node,
                "ref": i.port,
                "path": i.path,
                "blake3": i.blake3,
                "media_type": i.media_type,
                "size_bytes": i.size_bytes,
            });
            if let Some(md) = &i.metadata {
                o["metadata"] = md.clone();
            }
            o
        })
        .collect();
    let mut head = json!({
        "type": "start",
        "protocol": crate::manifest::PROTOCOL,
        "op": inv.op,
        "run_id": inv.run_id,
        "node_instance": inv.node_instance,
        "node_id": inv.node_id,
        "partition": inv.partition,
        "attempt": inv.attempt,
        "deadline_ms": inv.timeout_ms,
        "run_start_ms": crate::context::RunContext::of(inv).run_ms,
        "scratch": inv.scratch,
        "config": run.config,
        "inputs": inputs,
    });
    if let Some(w) = inv.window {
        head["window"] = json!({"start_ms": w.start_ms, "end_ms": w.end_ms});
    }
    if let Some(ws) = workspace {
        head["workspace"] = json!(ws);
    }
    if let Some(code) = &inv.code {
        let mut c = json!({"path": code.path, "blake3": code.blake3});
        if let Some(d) = code_dir {
            c["dir"] = json!(d);
        }
        head["code"] = c;
    }
    if let Some(cp) = &inv.checkpoint {
        head["checkpoint"] = cp.clone();
    }
    let head =
        serde_json::to_vec(&head).map_err(|_| internal("the start line cannot be serialised"))?;
    // `head` ends with `}`; the secrets member goes before it.
    let mut capacity = head.len() + 32;
    for (name, value) in run.secrets.iter() {
        capacity += 6 * (name.len() + value.len()) + 8;
    }
    let mut buf: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(capacity));
    buf.extend_from_slice(&head[..head.len() - 1]);
    buf.extend_from_slice(b",\"secrets\":{");
    for (n, (name, value)) in run.secrets.iter().enumerate() {
        if n > 0 {
            buf.push(b',');
        }
        serde_json::to_writer(&mut *buf, name).map_err(|_| internal("secret name"))?;
        buf.push(b':');
        serde_json::to_writer(&mut *buf, value).map_err(|_| internal("secret value"))?;
    }
    buf.extend_from_slice(b"}}\n");
    debug_assert!(buf.len() <= capacity);
    Ok(buf)
}

async fn run_inner(run: &ChildRun<'_>) -> OpResult<Outcome> {
    let prepared = prepare(run).await?;
    let m = &run.manifest.manifest;
    let scrubber = run.secrets.scrubber();
    let mut cmd = sandbox::command(&prepared.backend, &prepared.plan);
    let mut child = cmd.spawn().map_err(|e| {
        OpError::new(
            "op.tool_missing",
            format!("{} could not be started: {}", m.name, e.kind()),
        )
    })?;
    tracing::debug!(op = %m.name, sandbox = prepared.backend.name(), "child operator started");

    let mut stdin = child.stdin.take().ok_or_else(|| internal("child stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| internal("child stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| internal("child stderr"))?;
    let stderr_task = tokio::spawn(forward_stderr(
        stderr,
        scrubber_clone(run.secrets),
        run.ctx.events.clone(),
    ));
    let line = prepared.start_line;
    let write = async move {
        let r = stdin.write_all(&line).await;
        drop(line);
        let _ = stdin.shutdown().await;
        r
    };

    let deadline = Instant::now() + Duration::from_millis(run.inv.timeout_ms.max(1000));
    let mut cancel = run.ctx.cancel.clone();
    let mut reader = BufReader::new(stdout);
    let mut state = Collected::default();
    let mut write = Box::pin(write);
    let mut written = false;
    let mut cancel_live = true;
    let mut buf: Vec<u8> = Vec::new();
    let ended: End = loop {
        tokio::select! {
            biased;
            changed = cancel.changed(), if cancel_live => {
                match changed {
                    // The daemon dropped its sender: this attempt can no longer be cancelled.
                    Err(_) => cancel_live = false,
                    Ok(()) if *cancel.borrow() => break End::Cancelled,
                    Ok(()) => {}
                }
            }
            () = tokio::time::sleep_until(deadline) => break End::Timeout,
            r = &mut write, if !written => {
                written = true;
                if r.is_err() {
                    tracing::debug!(op = %m.name, "the child closed stdin before reading the start line");
                }
            }
            r = read_line_capped(&mut reader, &mut buf, MAX_LINE) => match r {
                Ok(0) => break End::Eof,
                Ok(_) => {
                    // A partial line stays in `buf` if another branch wins; it is cleared only here.
                    let handled = handle_line(&buf, run, &scrubber, &mut state);
                    buf.clear();
                    if let Err(e) = handled {
                        break End::Violation(e);
                    }
                }
                Err(_) => break End::Violation(OpError::new(
                    "op.protocol_violation",
                    format!("{} wrote a stdout line longer than 1 MiB or invalid bytes", m.name),
                )),
            }
        }
    };
    let outcome = match ended {
        End::Cancelled => {
            sandbox::kill_tree(&mut child).await;
            Ok(Outcome::Cancelled)
        }
        End::Timeout => {
            sandbox::kill_tree(&mut child).await;
            Err(OpError::new(
                "op.timeout",
                format!(
                    "{} exceeded the attempt's timeout of {} ms",
                    m.name, run.inv.timeout_ms
                ),
            ))
        }
        End::Violation(e) => {
            sandbox::kill_tree(&mut child).await;
            Err(e)
        }
        End::Eof => {
            let wait =
                tokio::time::timeout_at(deadline.min(Instant::now() + EXIT_GRACE), child.wait())
                    .await;
            let status = match wait {
                Ok(Ok(s)) => Some(s),
                _ => None,
            };
            sandbox::kill_tree(&mut child).await;
            finish(run, state, status)
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), stderr_task).await;
    outcome
}

fn scrubber_clone(secrets: &ResolvedSecrets) -> Scrubber {
    secrets.scrubber()
}

enum End {
    Eof,
    Cancelled,
    Timeout,
    Violation(OpError),
}

#[derive(Default)]
struct Collected {
    outputs: Vec<(String, String, Option<String>, Option<Value>)>,
    result_ok: bool,
    error: Option<OpError>,
    terminal: bool,
}

async fn read_line_capped<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<usize> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(buf.len());
        }
        if let Some(i) = available.iter().position(|b| *b == b'\n') {
            buf.extend_from_slice(&available[..=i]);
            reader.consume(i + 1);
            if buf.len() > cap {
                return Err(std::io::Error::other("line too long"));
            }
            return Ok(buf.len());
        }
        let n = available.len();
        buf.extend_from_slice(available);
        reader.consume(n);
        if buf.len() > cap {
            return Err(std::io::Error::other("line too long"));
        }
    }
}

fn handle_line(
    raw: &[u8],
    run: &ChildRun<'_>,
    scrubber: &Scrubber,
    state: &mut Collected,
) -> OpResult<()> {
    let m = &run.manifest.manifest;
    let text = std::str::from_utf8(raw).map_err(|_| {
        OpError::new(
            "op.protocol_violation",
            format!("{} wrote a stdout line that is not UTF-8", m.name),
        )
    })?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    let violation = |what: &str| {
        OpError::new(
            "op.protocol_violation",
            format!("{} broke jsonl-stdio/0: {what}", m.name),
        )
    };
    let line: Value =
        serde_json::from_str(text).map_err(|_| violation("a stdout line is not JSON"))?;
    let kind = line
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| violation("a line has no type"))?;
    if state.terminal && matches!(kind, "output" | "result" | "error") {
        return Err(violation("a line after result or error"));
    }
    let u64_of = |k: &str| line.get(k).and_then(Value::as_u64);
    match kind {
        "progress" => {
            let done = u64_of("done").ok_or_else(|| violation("progress without done"))?;
            send(
                run.ctx,
                OpEvent::Progress {
                    done,
                    total: u64_of("total"),
                },
            );
        }
        "output" => {
            let port = line
                .get("port")
                .and_then(Value::as_str)
                .ok_or_else(|| violation("output without port"))?;
            if m.output(port).is_none() {
                return Err(OpError::new(
                    "op.sandbox_violation",
                    format!(
                        "{} wrote to an output port its manifest does not declare",
                        m.name
                    ),
                ));
            }
            let path = line
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| violation("output without path"))?;
            let media_type = line
                .get("media_type")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let metadata = line.get("metadata").cloned().filter(|v| !v.is_null());
            if metadata.as_ref().is_some_and(|v| !v.is_object()) {
                return Err(violation("output metadata is not an object"));
            }
            if state.outputs.len() >= 100_000 {
                return Err(crate::error::limit(format!(
                    "{} reported more than 100000 outputs",
                    m.name
                )));
            }
            state
                .outputs
                .push((port.to_owned(), path.to_owned(), media_type, metadata));
        }
        "checkpoint" => {
            let cp = line
                .get("checkpoint")
                .or_else(|| line.get("data"))
                .cloned()
                .ok_or_else(|| violation("checkpoint without checkpoint"))?;
            let size = serde_json::to_vec(&cp)
                .map(|v| v.len())
                .unwrap_or(usize::MAX);
            if size > MAX_CHECKPOINT {
                return Err(violation("a checkpoint larger than 64 KiB"));
            }
            send(run.ctx, OpEvent::Checkpoint(cp));
        }
        "usage" => {
            let s = |k: &str| {
                line.get(k)
                    .and_then(Value::as_str)
                    .map(|v| v.chars().take(128).collect::<String>())
            };
            let (Some(provider), Some(model)) = (s("provider"), s("model")) else {
                return Err(violation("usage without provider or model"));
            };
            send(
                run.ctx,
                OpEvent::Usage {
                    provider,
                    model,
                    input_tokens: u64_of("input_tokens").unwrap_or(0),
                    output_tokens: u64_of("output_tokens").unwrap_or(0),
                    cost_usd_micro: u64_of("cost_usd_micro").unwrap_or(0),
                },
            );
        }
        "log" => {
            if let Some(msg) = line.get("message").and_then(Value::as_str) {
                let msg: String = msg.chars().take(MAX_STDERR_LINE).collect();
                send(run.ctx, OpEvent::Log(scrubber.scrub(&msg)));
            }
        }
        "result" => {
            state.terminal = true;
            if line.get("ok").and_then(Value::as_bool) == Some(true) {
                state.result_ok = true;
            } else {
                state.error = Some(OpError::new(
                    "op.operator_failed",
                    format!("{} reported a result that is not ok", m.name),
                ));
            }
        }
        "error" => {
            state.terminal = true;
            let envelope = line.get("error").filter(|e| e.is_object()).unwrap_or(&line);
            state.error = Some(map_child_error(envelope, scrubber, &m.name));
        }
        other => {
            tracing::debug!(op = %m.name, line_type = other, "ignored an unknown jsonl-stdio/0 line type");
        }
    }
    Ok(())
}

/// Maps a child's error envelope to a registered Ogun code. A registered `op.*`, `llm.*`, `secret.*` or
/// `artifact.*` code, or `run.budget_exhausted` (a model operator's refused budget check), is kept; anything else becomes `op.operator_failed` or, when the child marked it
/// retryable with a transient class, `op.operator_transient`, with the child's code in `details`.
fn map_child_error(env: &Value, scrubber: &Scrubber, op: &str) -> OpError {
    let code = env.get("code").and_then(Value::as_str).unwrap_or("");
    let class = env.get("class").and_then(Value::as_str).unwrap_or("");
    let retryable = env
        .get("retryable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let raw: String = env
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the operator failed")
        .chars()
        .take(900)
        .collect();
    let message = format!("{op}: {}", scrubber.scrub(&raw));
    let registered = ogun_types::code_info(code).is_some()
        && (["op.", "llm.", "secret.", "artifact."]
            .iter()
            .any(|p| code.starts_with(p))
            || code == "run.budget_exhausted");
    if registered {
        return OpError::new(code, message);
    }
    let lexical = code.len() <= 64
        && code.contains('.')
        && code.split('.').all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        });
    let transient = retryable
        && matches!(
            class,
            "transient_network" | "throttled" | "unavailable" | "timeout"
        );
    let mut details = Map::new();
    if lexical {
        details.insert("operator_code".into(), Value::String(code.to_owned()));
    }
    if class.len() <= 32
        && class.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        && !class.is_empty()
    {
        details.insert("operator_class".into(), Value::String(class.to_owned()));
    }
    let base = if transient {
        "op.operator_transient"
    } else {
        "op.operator_failed"
    };
    let e = OpError::new(base, message);
    if details.is_empty() {
        e
    } else {
        e.with_details(details)
    }
}

fn finish(
    run: &ChildRun<'_>,
    state: Collected,
    status: Option<std::process::ExitStatus>,
) -> OpResult<Outcome> {
    let m = &run.manifest.manifest;
    if let Some(e) = state.error {
        return Err(e);
    }
    if let Some(s) = status {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            // SIGXCPU, SIGXFSZ, SIGSYS: the sandbox's limits stopped the child.
            if let Some(sig) = s.signal()
                && matches!(sig, 24 | 25 | 31)
            {
                return Err(OpError::new(
                    "op.sandbox_violation",
                    format!(
                        "{} exceeded a CPU, file size or system call limit of its sandbox",
                        m.name
                    ),
                ));
            }
        }
        if state.result_ok && !s.success() {
            return Err(OpError::new(
                "op.crashed",
                format!("{} exited unsuccessfully ({s}) after its result", m.name),
            ));
        }
    }
    if !state.result_ok {
        let how = status.map_or_else(|| "did not exit".to_owned(), |s| s.to_string());
        return Err(OpError::new(
            "op.crashed",
            format!("{} ended without a result or error line ({how})", m.name),
        ));
    }
    let mut outputs = Vec::with_capacity(state.outputs.len());
    for (port, path, media_type, metadata) in state.outputs {
        let p = Path::new(&path);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            run.inv.scratch.join(p)
        };
        if !files::inside(&run.inv.scratch, &full) {
            return Err(OpError::new(
                "op.sandbox_violation",
                format!(
                    "{} reported an output outside its scratch directory",
                    m.name
                ),
            ));
        }
        let real = full
            .canonicalize()
            .map_err(|e| crate::error::io("resolving an output", &e))?;
        if !real.is_file() {
            return Err(OpError::new(
                "op.output_invalid",
                format!("{} reported an output that is not a regular file", m.name),
            ));
        }
        let name = real
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let media_type = media_type
            .filter(|mt| mt.contains('/') && mt.len() <= 255)
            .or_else(|| m.output(&port).and_then(|p| p.media_type.clone()))
            .unwrap_or_else(|| files::media_type_for(&name).to_owned());
        outputs.push(OutputFile {
            port,
            path: real,
            media_type,
            metadata: metadata.map(integerize),
        });
    }
    Ok(Outcome::Succeeded { outputs })
}

async fn forward_stderr(
    stderr: tokio::process::ChildStderr,
    scrubber: Scrubber,
    events: tokio::sync::mpsc::UnboundedSender<OpEvent>,
) {
    let mut reader = BufReader::new(stderr);
    let mut kept = 0usize;
    let mut truncated = false;
    let mut chunk = vec![0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if kept >= MAX_STDERR {
            truncated = true;
            continue; // keep draining so the child never blocks on a full pipe
        }
        pending.extend_from_slice(&chunk[..n]);
        while let Some(i) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=i).collect();
            emit_stderr(&line, &scrubber, &events, &mut kept);
        }
        if pending.len() > MAX_STDERR_LINE {
            let line: Vec<u8> = std::mem::take(&mut pending);
            emit_stderr(&line, &scrubber, &events, &mut kept);
        }
    }
    if !pending.is_empty() && kept < MAX_STDERR {
        emit_stderr(&pending, &scrubber, &events, &mut kept);
    }
    if truncated {
        let _ = events.send(OpEvent::Log(
            "stderr: further output dropped (64 KiB per attempt)".into(),
        ));
    }
}

fn emit_stderr(
    line: &[u8],
    scrubber: &Scrubber,
    events: &tokio::sync::mpsc::UnboundedSender<OpEvent>,
    kept: &mut usize,
) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end();
    if text.is_empty() {
        return;
    }
    let cut: String = text.chars().take(MAX_STDERR_LINE).collect();
    let clean = scrubber.scrub(&cut);
    *kept += clean.len();
    let _ = events.send(OpEvent::Log(format!("stderr: {clean}")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_errors_map_to_registered_codes() {
        let s = Scrubber::default();
        let e = map_child_error(
            &json!({"code": "my.thing", "class": "unavailable", "message": "x", "retryable": true}),
            &s,
            "op",
        );
        assert_eq!(e.error.code, "op.operator_transient");
        assert!(e.retryable);
        let e = map_child_error(
            &json!({"code": "op.tool_failed", "class": "whatever", "message": "x"}),
            &s,
            "op",
        );
        assert_eq!(e.error.code, "op.tool_failed");
        let e = map_child_error(&json!({"code": "approval.expired"}), &s, "op");
        assert_eq!(e.error.code, "op.operator_failed");
    }
}
