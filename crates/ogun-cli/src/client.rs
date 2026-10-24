//! Commands that talk to `ogund` through the O-15 client.
//!
//! The CLI connects with the credential named by `--credential` (default `cli`, the `human`
//! credential `ogund` creates on first start). Approvals and other trust-changing commands read the
//! person's confirmation from the controlling terminal (`/dev/tty`, or `CONIN$` on Windows), never
//! from stdin, so a program that pipes into `ogun` cannot answer for the person (Zeus ADR 0007).

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use clap::Subcommand;
use ogun_api::auth::ClientKind;
use ogun_api::methods::{
    self as m, ApprovalState, ApprovalView, Decision, Evidence, PartitionRange, PartitionSelector,
    RunView, name,
};
use ogun_api::{Client, OgunPaths};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Daemon-backed commands.
#[derive(Debug, Subcommand)]
pub enum DaemonCmd {
    /// Start ogund in the background (if it is not running), wait until it answers.
    Start,
    /// Show the daemon's status.
    Status,
    /// Stop the daemon (needs the CLI on a terminal).
    Stop,
    /// Print where ogund keeps its state and where it listens.
    Paths,
}

/// Credential management.
#[derive(Debug, Subcommand)]
pub enum CredCmd {
    /// Create a credential; its token file is written under the state directory.
    Add {
        /// Name, `[a-z0-9_-]`.
        name: String,
        /// `human`, `agent`, `service` or `bridge`.
        #[arg(long, default_value = "agent")]
        kind: String,
    },
    /// List credentials (never their tokens).
    List,
    /// Revoke a credential.
    Revoke {
        /// `ocl_...` id.
        id: String,
    },
}

/// Secret management.
#[derive(Debug, Subcommand)]
pub enum SecretCmd {
    /// Store a secret for `{"secret_ref": NAME}`; the value is read from stdin when piped, else typed
    /// on the terminal without echo.
    Set {
        /// Secret name.
        name: String,
    },
    /// List secret names.
    List,
    /// Delete a secret.
    Rm {
        /// Secret name.
        name: String,
    },
}

/// WebAuthn approver keys proposed by a server.
#[derive(Debug, Subcommand)]
pub enum ApproverCmd {
    /// List approver keys.
    List,
    /// Confirm a proposed key by the fingerprint shown on the server's page.
    Confirm {
        /// WebAuthn credential id.
        credential_id: String,
        /// Fingerprint (16 hex digits).
        fingerprint: String,
    },
    /// Revoke a key.
    Revoke {
        /// WebAuthn credential id.
        credential_id: String,
    },
}

/// Edge enrolment (hub side).
#[derive(Debug, Subcommand)]
pub enum EdgeCmd {
    /// Enrol an edge's Ed25519 public key (from `ogun-sync keygen` on the edge).
    Enroll {
        /// Label.
        name: String,
        /// Public key, base64url.
        public_key: String,
    },
    /// List enrolled edges.
    List,
}

/// Options shared by the client commands.
#[derive(Debug, Clone)]
pub struct Opts {
    /// Credential file name.
    pub credential: String,
    /// Profile.
    pub profile: Option<String>,
    /// JSON output.
    pub json: bool,
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")
}

fn paths(o: &Opts) -> Result<OgunPaths> {
    OgunPaths::discover(o.profile.as_deref()).map_err(|e| anyhow!("{e}"))
}

async fn connect(o: &Opts) -> Result<Client> {
    let p = paths(o)?;
    Client::connect(
        &p,
        &o.credential,
        m::ClientInfo {
            name: "ogun".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await
    .map_err(|e| anyhow!("{e} (is ogund running? try `ogun daemon start`)"))
}

async fn call<P: Serialize, R: DeserializeOwned>(c: &mut Client, method: &str, p: &P) -> Result<R> {
    c.call(method, p).await.map_err(|e| anyhow!("{e}"))
}

fn print_json(v: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn fresh_key(prefix: &str) -> String {
    format!("{prefix}-{}", ulid_like())
}

fn ulid_like() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    format!("{now:x}{}", u64::from_le_bytes(bytes))
}

/// Reads one line from the controlling terminal, never stdin.
fn read_tty_line(prompt: &str) -> Result<String> {
    #[cfg(windows)]
    let (input, output) = ("CONIN$", "CONOUT$");
    #[cfg(not(windows))]
    let (input, output) = ("/dev/tty", "/dev/tty");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .open(output)
        .context("no controlling terminal")?;
    write!(out, "{prompt}")?;
    out.flush()?;
    let tty = std::fs::File::open(input).context("no controlling terminal")?;
    let mut line = String::new();
    std::io::BufReader::new(tty).read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// Reads a secret: stdin when piped, else the terminal without echo.
fn read_secret() -> Result<zeroize::Zeroizing<String>> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        return Ok(zeroize::Zeroizing::new(
            s.trim_end_matches(['\r', '\n']).to_owned(),
        ));
    }
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    eprint!("value (not shown): ");
    crossterm::terminal::enable_raw_mode()?;
    let mut value = zeroize::Zeroizing::new(String::new());
    let result = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow!("cancelled"));
                }
                KeyCode::Char(ch) => value.push(ch),
                KeyCode::Backspace => {
                    value.pop();
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    eprintln!();
    result.map(|()| value)
}

fn show_ranges(ranges: &[PartitionRange]) {
    for r in ranges {
        let span = if r.from == r.to {
            r.from.clone()
        } else {
            format!("{} .. {}", r.from, r.to)
        };
        let span = if span.is_empty() {
            "(the one partition)".to_owned()
        } else {
            span
        };
        println!(
            "  {:<8} {:>7}  {span}",
            format!("{:?}", r.state).to_lowercase(),
            r.count
        );
    }
}

fn show_run(r: &RunView) {
    println!(
        "{}  {} v{}  {:?}  {} partition(s)  created {}",
        r.run_id,
        r.workflow,
        r.version,
        r.state,
        r.partition_count,
        r.created_at.to_rfc3339()
    );
    for n in &r.nodes {
        let part = if n.partition.is_empty() {
            String::new()
        } else {
            format!(" [{}]", n.partition)
        };
        let cause = n
            .last_cause
            .map(|c| format!(" ({c:?})"))
            .unwrap_or_default();
        println!(
            "  {}{part}  {:?}{cause}  attempt {}",
            n.node, n.state, n.attempt
        );
    }
}

fn show_approval(a: &ApprovalView) {
    println!("{}  {:?}  {:?}", a.approval_id, a.kind, a.state);
    println!("  workflow      {}", a.workflow);
    println!("  workflow hash {}", a.workflow_hash);
    if let Some(r) = a.run_id {
        println!("  run           {r}");
    }
    if let Some(n) = &a.node {
        println!("  node          {n}");
    }
    for f in &a.facts {
        println!("  {:<13} {}", f.label, f.value);
    }
    println!("  requested by  {}", a.requested_by);
    println!("  expires       {}", a.expires_at.to_rfc3339());
    println!("  request hash  {}", a.request_hash);
}

/// Collects the code bundles a workflow file references by `code.path`. The path is relative to the
/// workspace (O-02 section 7): the current directory, or the nearest directory above the file that
/// contains it.
fn code_bundles(file: &Path, doc: &Value) -> Result<BTreeMap<String, String>> {
    let base = file.parent().unwrap_or(Path::new("."));
    let resolve = |rel: &str| -> Option<PathBuf> {
        std::iter::once(PathBuf::from(rel))
            .chain(base.ancestors().map(|a| a.join(rel)))
            .find(|p| p.is_file())
    };
    let mut out = BTreeMap::new();
    for node in doc
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(code) = node.get("code") else {
            continue;
        };
        let (Some(hash), Some(rel)) = (
            code.get("blake3").and_then(Value::as_str),
            code.get("path").and_then(Value::as_str),
        ) else {
            continue;
        };
        let Some(path) = resolve(rel) else { continue };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            bail!(
                "{} does not match its code.blake3 in the workflow",
                path.display()
            );
        }
        out.insert(
            hash.to_owned(),
            base64::engine::general_purpose::STANDARD.encode(bytes),
        );
    }
    Ok(out)
}

/// `ogun daemon ...`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn daemon(o: &Opts, cmd: &DaemonCmd) -> Result<bool> {
    let rt = runtime()?;
    match cmd {
        DaemonCmd::Paths => {
            let p = paths(o)?;
            println!("state    {}", p.state_dir.display());
            println!("endpoint {}", p.endpoint());
            Ok(true)
        }
        DaemonCmd::Start => rt.block_on(async {
            if connect(o).await.is_ok() {
                println!("ogund is already running");
                return Ok(true);
            }
            let exe = std::env::current_exe()?;
            let ogund = exe.with_file_name(format!("ogund{}", std::env::consts::EXE_SUFFIX));
            let p = paths(o)?;
            std::fs::create_dir_all(&p.state_dir)?;
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p.state_dir.join("ogund.log"))?;
            let mut cmd = std::process::Command::new(&ogund);
            if let Some(profile) = &o.profile {
                cmd.arg("--profile").arg(profile);
            }
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(log);
            cmd.spawn()
                .with_context(|| format!("cannot start {}", ogund.display()))?;
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if connect(o).await.is_ok() {
                    println!(
                        "ogund started (log: {})",
                        p.state_dir.join("ogund.log").display()
                    );
                    return Ok(true);
                }
            }
            bail!(
                "ogund did not answer within 5 s; see {}",
                p.state_dir.join("ogund.log").display()
            )
        }),
        DaemonCmd::Status => rt.block_on(async {
            let mut c = connect(o).await?;
            let s: m::StatusResult = call(&mut c, name::DAEMON_STATUS, &json!({})).await?;
            if o.json {
                return print_json(&s).map(|()| true);
            }
            println!(
                "ogund {}  up since {}",
                s.daemon_version,
                s.started_at.to_rfc3339()
            );
            println!("  workflows         {}", s.workflows);
            println!("  active runs       {}", s.runs_active);
            println!("  pending approvals {}", s.approvals_pending);
            println!("  streams           {}", s.streams);
            println!(
                "  scheduler         {}",
                if s.scheduler_enabled { "on" } else { "off" }
            );
            println!(
                "  connection        {} ({})",
                c.kind(),
                if c.interactive() {
                    "interactive"
                } else {
                    "not interactive"
                }
            );
            Ok(true)
        }),
        DaemonCmd::Stop => rt.block_on(async {
            let mut c = connect(o).await?;
            let _: Value = call(&mut c, name::DAEMON_SHUTDOWN, &json!({})).await?;
            println!("ogund is stopping");
            Ok(true)
        }),
    }
}

/// `ogun put FILE`.
///
/// # Errors
///
/// I/O, connection or daemon errors.
pub fn put(o: &Opts, file: &Path) -> Result<bool> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let doc: Value = serde_json::from_str(&text).context("the workflow is not JSON")?;
    let code = code_bundles(file, &doc)?;
    let key = format!(
        "put-{}",
        &blake3::hash(text.as_bytes()).to_hex().as_str()[..32]
    );
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: m::PutWorkflowResult = call(
            &mut c,
            name::WORKFLOWS_PUT,
            &m::PutWorkflowParams {
                workflow: doc,
                idempotency_key: key,
                code,
            },
        )
        .await?;
        if o.json {
            return print_json(&r).map(|()| true);
        }
        println!(
            "{} v{} {}  {}",
            r.name,
            r.version,
            if r.created {
                "stored"
            } else {
                "already stored"
            },
            r.workflow_hash
        );
        for d in &r.diagnostics {
            println!("  warning {} at {}: {}", d.code, d.path, d.message);
        }
        if r.approvals.is_empty() {
            println!("  runnable now");
        } else {
            println!("  needs approval before it runs:");
            for a in &r.approvals {
                println!(
                    "    {}  {:?}   ogun approve {}",
                    a.approval_id, a.kind, a.approval_id
                );
            }
        }
        Ok(true)
    })
}

/// `ogun workflows`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn workflows(o: &Opts) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: m::ListWorkflowsResult = call(&mut c, name::WORKFLOWS_LIST, &json!({})).await?;
        if o.json {
            return print_json(&r).map(|()| true);
        }
        for w in &r.workflows {
            println!(
                "{:<32} v{:<4} {}  {}{}",
                w.name,
                w.latest_version,
                &w.workflow_hash[..12],
                if w.enabled { "enabled" } else { "paused" },
                if w.runnable { "" } else { "  (needs approval)" }
            );
        }
        Ok(true)
    })
}

/// What `ogun run` starts.
#[derive(Debug, Clone)]
pub struct RunArgs {
    /// Workflow name.
    pub workflow: String,
    /// Version.
    pub version: Option<u64>,
    /// Partition keys.
    pub partitions: Vec<String>,
    /// Range start.
    pub from: Option<String>,
    /// Range end.
    pub to: Option<String>,
    /// Re-run up-to-date partitions.
    pub force: bool,
    /// Wait for the run to finish.
    pub wait: bool,
    /// Idempotency key.
    pub idempotency_key: Option<String>,
}

async fn wait_run(c: &mut Client, run: &RunView, json_out: bool) -> Result<bool> {
    let mut after = 0u64;
    loop {
        let r: m::EventsSinceResult = call(
            c,
            name::EVENTS_SINCE,
            &m::EventsSinceParams {
                after,
                run_id: Some(run.run_id),
                limit: Some(500),
                wait_ms: 10_000,
            },
        )
        .await?;
        after = r.last_seq;
        if !json_out {
            for e in &r.events {
                if let Some(kind) = e.kind.get("to") {
                    let i = e.instance_id.map(|i| i.to_string()).unwrap_or_default();
                    println!(
                        "  {}  {i}  -> {}",
                        e.at.to_rfc3339(),
                        kind.as_str().unwrap_or("")
                    );
                }
            }
        }
        let view: RunView = call(c, name::RUNS_GET, &m::RunRef { run_id: run.run_id }).await?;
        use ogun_run::RunState as S;
        match view.state {
            S::Succeeded | S::Failed | S::Cancelled => {
                if json_out {
                    print_json(&view)?;
                } else {
                    show_run(&view);
                }
                return Ok(view.state == S::Succeeded);
            }
            S::Waiting => {
                if !json_out {
                    println!("  waiting: see `ogun approvals`");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            _ => {}
        }
    }
}

/// `ogun run`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn run(o: &Opts, a: &RunArgs) -> Result<bool> {
    let partitions = match (&a.from, &a.to, a.partitions.is_empty()) {
        (Some(f), Some(t), _) => PartitionSelector::Range {
            from: f.clone(),
            to: t.clone(),
            force: a.force,
        },
        (None, None, false) => PartitionSelector::Keys {
            keys: a.partitions.clone(),
        },
        (None, None, true) => PartitionSelector::Latest,
        _ => bail!("--from and --to go together"),
    };
    let p = m::StartRunParams {
        workflow: a.workflow.clone(),
        version: a.version,
        workflow_hash: None,
        partitions,
        idempotency_key: a
            .idempotency_key
            .clone()
            .unwrap_or_else(|| fresh_key("run")),
    };
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: RunView = call(&mut c, name::RUNS_START, &p).await?;
        if !o.json {
            println!("{}  started, {} partition(s)", r.run_id, r.partition_count);
        }
        if a.wait {
            return wait_run(&mut c, &r, o.json).await;
        }
        if o.json {
            print_json(&r)?;
        }
        Ok(true)
    })
}

/// `ogun backfill`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn backfill(
    o: &Opts,
    workflow: &str,
    from: &str,
    to: &str,
    force: bool,
    yes: bool,
) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let plan: m::PlanBackfillResult = call(
            &mut c,
            name::BACKFILLS_PLAN,
            &m::PlanBackfillParams {
                workflow: workflow.into(),
                from: from.into(),
                to: to.into(),
            },
        )
        .await?;
        if o.json && !yes {
            return print_json(&plan).map(|()| true);
        }
        println!(
            "{workflow} {from} .. {to}: {} to run, {} up to date",
            plan.to_run, plan.up_to_date
        );
        show_ranges(&plan.ranges);
        if !yes {
            println!("dry run; pass --yes to start it");
            return Ok(true);
        }
        let r: RunView = call(
            &mut c,
            name::RUNS_START,
            &m::StartRunParams {
                workflow: workflow.into(),
                version: None,
                workflow_hash: Some(plan.workflow_hash.clone()),
                partitions: PartitionSelector::Range {
                    from: from.into(),
                    to: to.into(),
                    force,
                },
                idempotency_key: fresh_key("backfill"),
            },
        )
        .await?;
        println!("{}  started, {} partition(s)", r.run_id, r.partition_count);
        Ok(true)
    })
}

/// `ogun partition-status`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn partition_status(
    o: &Opts,
    workflow: &str,
    from: Option<String>,
    to: Option<String>,
) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: m::PartitionStatusResult = call(
            &mut c,
            name::PARTITIONS_STATUS,
            &m::PartitionStatusParams {
                workflow: workflow.into(),
                version: None,
                from,
                to,
            },
        )
        .await?;
        if o.json {
            return print_json(&r).map(|()| true);
        }
        println!("{workflow}  {}", r.workflow_hash);
        show_ranges(&r.ranges);
        Ok(true)
    })
}

/// `ogun runs`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn runs(o: &Opts, workflow: Option<String>, limit: u32) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: m::ListRunsResult = call(
            &mut c,
            name::RUNS_LIST,
            &m::ListRunsParams {
                workflow,
                before: None,
                limit: Some(limit),
            },
        )
        .await?;
        if o.json {
            return print_json(&r).map(|()| true);
        }
        for run in &r.runs {
            show_run(run);
        }
        Ok(true)
    })
}

/// `ogun show RUN`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn show(o: &Opts, run_id: &str) -> Result<bool> {
    let run_id = run_id
        .parse()
        .map_err(|_| anyhow!("not a run id (orun_...)"))?;
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: RunView = call(&mut c, name::RUNS_GET, &m::RunRef { run_id }).await?;
        if o.json {
            return print_json(&r).map(|()| true);
        }
        show_run(&r);
        Ok(true)
    })
}

/// `ogun cancel RUN`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn cancel(o: &Opts, run_id: &str) -> Result<bool> {
    let run_id = run_id
        .parse()
        .map_err(|_| anyhow!("not a run id (orun_...)"))?;
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: RunView = call(
            &mut c,
            name::RUNS_CANCEL,
            &m::CancelRunParams {
                run_id,
                idempotency_key: fresh_key("cancel"),
            },
        )
        .await?;
        show_run(&r);
        Ok(true)
    })
}

/// `ogun approvals`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn approvals(o: &Opts, all: bool) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let state = if all {
            None
        } else {
            Some(ApprovalState::Pending)
        };
        let r: m::ListApprovalsResult = call(
            &mut c,
            name::APPROVALS_LIST,
            &m::ListApprovalsParams { state },
        )
        .await?;
        if o.json {
            return print_json(&r).map(|()| true);
        }
        if r.approvals.is_empty() {
            println!("no {}approvals", if all { "" } else { "pending " });
        }
        for a in &r.approvals {
            show_approval(a);
            println!();
        }
        Ok(true)
    })
}

/// `ogun approve ID` / `ogun deny ID`.
///
/// # Errors
///
/// Connection errors, a declined confirmation, or the daemon's refusal.
pub fn decide(o: &Opts, approval_id: &str, decision: Decision) -> Result<bool> {
    let approval_id = approval_id
        .parse()
        .map_err(|_| anyhow!("not an approval id (oap_...)"))?;
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        if c.kind() != ClientKind::Human || !c.interactive() {
            bail!("decisions need the ogun CLI on a terminal with a human credential");
        }
        let a: ApprovalView =
            call(&mut c, name::APPROVALS_GET, &m::ApprovalRef { approval_id }).await?;
        show_approval(&a);
        let verb = match decision {
            Decision::Approve => "approve",
            Decision::Deny => "deny",
        };
        let code = &a.request_hash[..8];
        let typed = read_tty_line(&format!("\nType {code} to {verb} this request: "))?;
        if typed != code {
            bail!("confirmation did not match; nothing was decided");
        }
        let r: ApprovalView = call(
            &mut c,
            name::APPROVALS_DECIDE,
            &m::DecideParams {
                approval_id,
                decision,
                request_hash: a.request_hash.clone(),
                evidence: Evidence::Terminal,
            },
        )
        .await?;
        println!("{}  {:?}", r.approval_id, r.state);
        Ok(true)
    })
}

/// `ogun credentials ...`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn credentials(o: &Opts, cmd: &CredCmd) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        match cmd {
            CredCmd::Add { name: n, kind } => {
                let kind: ClientKind = kind.parse().map_err(|e| anyhow!("{e}"))?;
                let r: m::AddCredentialResult = call(
                    &mut c,
                    name::CREDENTIALS_ADD,
                    &m::AddCredentialParams {
                        name: n.clone(),
                        kind,
                    },
                )
                .await?;
                println!(
                    "{}  {}  {}  token file {}",
                    r.credential.credential_id, r.credential.kind, r.credential.name, r.token_file
                );
            }
            CredCmd::List => {
                let r: m::ListCredentialsResult =
                    call(&mut c, name::CREDENTIALS_LIST, &json!({})).await?;
                if o.json {
                    return print_json(&r).map(|()| true);
                }
                for x in &r.credentials {
                    println!(
                        "{}  {:<8} {:<16} {}{}",
                        x.credential_id,
                        x.kind,
                        x.name,
                        x.created_at.to_rfc3339(),
                        if x.revoked { "  revoked" } else { "" }
                    );
                }
            }
            CredCmd::Revoke { id } => {
                let credential_id = id
                    .parse()
                    .map_err(|_| anyhow!("not a credential id (ocl_...)"))?;
                let r: m::CredentialView = call(
                    &mut c,
                    name::CREDENTIALS_REVOKE,
                    &m::RevokeCredentialParams { credential_id },
                )
                .await?;
                println!("{} revoked", r.credential_id);
            }
        }
        Ok(true)
    })
}

/// `ogun secrets ...`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn secrets(o: &Opts, cmd: &SecretCmd) -> Result<bool> {
    let value = match cmd {
        SecretCmd::Set { .. } => Some(read_secret()?),
        _ => None,
    };
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        match cmd {
            SecretCmd::Set { name: n } => {
                let v = value.ok_or_else(|| anyhow!("no value"))?;
                let _: Value = call(
                    &mut c,
                    name::SECRETS_PUT,
                    &m::PutSecretParams {
                        name: n.clone(),
                        value: v.to_string(),
                    },
                )
                .await?;
                println!("secret {n} stored");
            }
            SecretCmd::List => {
                let r: m::ListSecretsResult = call(&mut c, name::SECRETS_LIST, &json!({})).await?;
                for n in &r.names {
                    println!("{n}");
                }
            }
            SecretCmd::Rm { name: n } => {
                let _: Value = call(
                    &mut c,
                    name::SECRETS_DELETE,
                    &m::SecretRef { name: n.clone() },
                )
                .await?;
                println!("secret {n} deleted");
            }
        }
        Ok(true)
    })
}

/// `ogun approvers ...`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn approvers(o: &Opts, cmd: &ApproverCmd) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        match cmd {
            ApproverCmd::List => {
                let r: m::ListApproversResult =
                    call(&mut c, name::APPROVERS_LIST, &json!({})).await?;
                for a in &r.approvers {
                    println!(
                        "{}  user {}  rp {}  fingerprint {}  {}",
                        a.credential_id,
                        a.user,
                        a.rp_id,
                        a.fingerprint,
                        if a.confirmed { "confirmed" } else { "PROPOSED" }
                    );
                }
            }
            ApproverCmd::Confirm {
                credential_id,
                fingerprint,
            } => {
                let r: m::ApproverView = call(
                    &mut c,
                    name::APPROVERS_CONFIRM,
                    &m::ApproverRef {
                        credential_id: credential_id.clone(),
                        fingerprint: Some(fingerprint.clone()),
                    },
                )
                .await?;
                println!("approver key for {} confirmed", r.user);
            }
            ApproverCmd::Revoke { credential_id } => {
                let r: m::ApproverView = call(
                    &mut c,
                    name::APPROVERS_REVOKE,
                    &m::ApproverRef {
                        credential_id: credential_id.clone(),
                        fingerprint: None,
                    },
                )
                .await?;
                println!("approver key for {} revoked", r.user);
            }
        }
        Ok(true)
    })
}

/// `ogun edges ...`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn edges(o: &Opts, cmd: &EdgeCmd) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        match cmd {
            EdgeCmd::Enroll {
                name: n,
                public_key,
            } => {
                let r: m::EdgeView = call(
                    &mut c,
                    name::EDGES_ENROLL,
                    &m::EnrollEdgeParams {
                        name: n.clone(),
                        public_key: public_key.clone(),
                    },
                )
                .await?;
                println!("{}  {} enrolled", r.edge_id, r.name);
            }
            EdgeCmd::List => {
                let r: m::ListEdgesResult = call(&mut c, name::EDGES_LIST, &json!({})).await?;
                for e in &r.edges {
                    println!(
                        "{}  {:<16} enrolled {}  last sync {}",
                        e.edge_id,
                        e.name,
                        e.enrolled_at.to_rfc3339(),
                        e.last_sync_at
                            .map_or_else(|| "never".into(), |t| t.to_rfc3339())
                    );
                }
            }
        }
        Ok(true)
    })
}

/// `ogun events`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn events(o: &Opts, after: u64, follow: bool, run: Option<String>) -> Result<bool> {
    let run_id = run
        .map(|r| r.parse().map_err(|_| anyhow!("not a run id")))
        .transpose()?;
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let mut after = after;
        loop {
            let r: m::EventsSinceResult = call(
                &mut c,
                name::EVENTS_SINCE,
                &m::EventsSinceParams {
                    after,
                    run_id,
                    limit: Some(1_000),
                    wait_ms: if follow { 30_000 } else { 0 },
                },
            )
            .await?;
            for e in &r.events {
                println!("{}", serde_json::to_string(e)?);
            }
            after = r.last_seq;
            if !follow {
                return Ok(true);
            }
        }
    })
}

/// `ogun artifacts RUN`.
///
/// # Errors
///
/// Connection or daemon errors.
pub fn artifacts(o: &Opts, run_id: &str) -> Result<bool> {
    let run_id = run_id
        .parse()
        .map_err(|_| anyhow!("not a run id (orun_...)"))?;
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: m::ListArtifactsResult = call(
            &mut c,
            name::ARTIFACTS_LIST,
            &m::ListArtifactsParams { run_id },
        )
        .await?;
        if o.json {
            return print_json(&r).map(|()| true);
        }
        for a in &r.artifacts {
            let part = if a.partition.is_empty() {
                String::new()
            } else {
                format!(" [{}]", a.partition)
            };
            println!(
                "{}  {:>10}  {}.{}{part}  {}",
                a.blake3, a.size_bytes, a.node, a.output, a.media_type
            );
        }
        Ok(true)
    })
}

/// `ogun artifact DIGEST`: prints the local path (or the bytes with `--cat`).
///
/// # Errors
///
/// Connection or daemon errors.
pub fn artifact(o: &Opts, blake3: &str, cat: bool) -> Result<bool> {
    runtime()?.block_on(async {
        let mut c = connect(o).await?;
        let r: m::GetArtifactResult = call(
            &mut c,
            name::ARTIFACTS_GET,
            &m::GetArtifactParams {
                blake3: blake3.into(),
                inline_max_bytes: 0,
            },
        )
        .await?;
        if cat {
            let bytes = std::fs::read(PathBuf::from(&r.path))?;
            std::io::stdout().write_all(&bytes)?;
        } else if o.json {
            print_json(&r)?;
        } else {
            println!("{}", r.path);
        }
        Ok(true)
    })
}
