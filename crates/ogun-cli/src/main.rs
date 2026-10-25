//! `ogun`, the Ogun command-line interface.
//!
//! Two groups of commands. Offline ones need no daemon: `validate`, `hash`, `diff`, `partitions`,
//! `next` and `hash-file` read workflow IR files and print what the runtime would accept, the hash
//! an approval would bind to, and what changed between two versions. The rest talk to `ogund`
//! through the O-15 client ([`client`]): store workflows, start and watch runs, backfill, see
//! partition coverage, and decide approvals on the terminal. Exit status: 0 success, 1 invalid,
//! failed or approval needed, 2 usage, I/O or connection error.

#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod client;
mod mcp;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ogun_ir::{Catalogue, Report, Severity, check, diff};

#[derive(Parser)]
#[command(
    name = "ogun",
    version,
    about = "Ogun: durable, reproducible data workflows"
)]
struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// The credential to connect to ogund with (a file under the state directory).
    #[arg(long, global = true, default_value = "cli", env = "OGUN_CREDENTIAL")]
    credential: String,
    /// Use a separate ogund profile.
    #[arg(long, global = true, env = "OGUN_PROFILE")]
    profile: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and validate a workflow IR file; print diagnostics, the hash and a summary.
    Validate {
        /// Workflow IR, JSON.
        file: PathBuf,
    },
    /// Print the workflow hash of a valid workflow IR file.
    Hash {
        /// Workflow IR, JSON.
        file: PathBuf,
    },
    /// List the complete partitions of a workflow and which still need a run.
    Partitions {
        /// Workflow IR, JSON.
        file: PathBuf,
        /// First partition key of a backfill range.
        #[arg(long, requires = "to")]
        from: Option<String>,
        /// Last partition key of a backfill range, inclusive.
        #[arg(long, requires = "from")]
        to: Option<String>,
        /// A run store to read finished partitions from.
        #[arg(long)]
        store: Option<PathBuf>,
    },
    /// Print when the workflow's triggers next start a run (UTC).
    Next {
        /// Workflow IR, JSON.
        file: PathBuf,
    },
    /// Print the BLAKE3 of a file, for a code operator's `code.blake3` (O-02 section 7).
    HashFile {
        /// The code bundle or script.
        file: PathBuf,
    },
    /// Serve the read-only Ogun tools to a coding agent over MCP on stdio.
    Mcp,
    /// Show what changed between two versions and whether the new one needs an approval.
    Diff {
        /// The approved version.
        old: PathBuf,
        /// The proposed version.
        new: PathBuf,
    },
    /// Start, stop and inspect ogund.
    #[command(subcommand)]
    Daemon(client::DaemonCmd),
    /// Store a workflow version in ogund (with the code bundles its `code.path` members name).
    Put {
        /// Workflow IR, JSON.
        file: PathBuf,
    },
    /// List the workflows ogund holds.
    Workflows,
    /// Start a run: the latest partition, named keys, or a range.
    Run {
        /// Workflow name.
        workflow: String,
        /// Version (default: latest).
        #[arg(long)]
        version: Option<u64>,
        /// Partition key; repeatable.
        #[arg(long = "partition", short = 'p')]
        partitions: Vec<String>,
        /// First key of a range.
        #[arg(long, requires = "to")]
        from: Option<String>,
        /// Last key of a range, inclusive.
        #[arg(long, requires = "from")]
        to: Option<String>,
        /// Re-run partitions already up to date.
        #[arg(long)]
        force: bool,
        /// Wait for the run to finish and print its node instances.
        #[arg(long)]
        wait: bool,
        /// Idempotency key (default: a fresh one).
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Plan a backfill over a range and, with --yes, start it.
    Backfill {
        /// Workflow name.
        workflow: String,
        /// First key.
        #[arg(long)]
        from: String,
        /// Last key, inclusive.
        #[arg(long)]
        to: String,
        /// Re-run partitions already up to date.
        #[arg(long)]
        force: bool,
        /// Start it (without this the plan is only printed).
        #[arg(long)]
        yes: bool,
    },
    /// Range-compressed partition coverage of a workflow.
    PartitionStatus {
        /// Workflow name.
        workflow: String,
        /// First key.
        #[arg(long)]
        from: Option<String>,
        /// Last key.
        #[arg(long)]
        to: Option<String>,
    },
    /// List runs, newest first.
    Runs {
        /// Only this workflow.
        #[arg(long)]
        workflow: Option<String>,
        /// At most this many.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Show a run and its node instances.
    Show {
        /// Run id (`orun_...`).
        run: String,
    },
    /// Cancel a run.
    Cancel {
        /// Run id (`orun_...`).
        run: String,
    },
    /// List approvals (pending by default).
    Approvals {
        /// Include decided and expired ones.
        #[arg(long)]
        all: bool,
    },
    /// Approve a request after reading its facts; you type a confirmation on this terminal.
    Approve {
        /// Approval id (`oap_...`).
        id: String,
    },
    /// Deny a request; you type a confirmation on this terminal.
    Deny {
        /// Approval id (`oap_...`).
        id: String,
    },
    /// Manage ogund's local credentials.
    #[command(subcommand)]
    Credentials(client::CredCmd),
    /// Manage secrets for `secret_ref`.
    #[command(subcommand)]
    Secrets(client::SecretCmd),
    /// Manage WebAuthn approver keys proposed by an Ogun server.
    #[command(subcommand)]
    Approvers(client::ApproverCmd),
    /// Enrol and list edges (on a hub).
    #[command(subcommand)]
    Edges(client::EdgeCmd),
    /// Print events as JSON lines.
    Events {
        /// Start after this sequence number.
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Keep waiting for new events.
        #[arg(long)]
        follow: bool,
        /// Only this run.
        #[arg(long)]
        run: Option<String>,
    },
    /// List the artifacts a run produced.
    Artifacts {
        /// Run id.
        run: String,
    },
    /// Print an artifact's local path, or its bytes with --cat.
    Artifact {
        /// BLAKE3 digest.
        blake3: String,
        /// Write the bytes to stdout.
        #[arg(long)]
        cat: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn load(path: &Path) -> Result<Report> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(check(&text, &Catalogue::builtin()))
}

fn print_diagnostics(path: &Path, report: &Report) {
    for d in &report.diagnostics {
        let level = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let at = if d.path.is_empty() {
            String::new()
        } else {
            format!(" at {}", d.path)
        };
        println!("{}: {level} {}{at}: {}", path.display(), d.code, d.message);
    }
}

fn run(cli: &Cli) -> Result<bool> {
    let o = client::Opts {
        credential: cli.credential.clone(),
        profile: cli.profile.clone(),
        json: cli.json,
    };
    match &cli.command {
        Command::Daemon(cmd) => client::daemon(&o, cmd),
        Command::Put { file } => client::put(&o, file),
        Command::Workflows => client::workflows(&o),
        Command::Run {
            workflow,
            version,
            partitions,
            from,
            to,
            force,
            wait,
            idempotency_key,
        } => client::run(
            &o,
            &client::RunArgs {
                workflow: workflow.clone(),
                version: *version,
                partitions: partitions.clone(),
                from: from.clone(),
                to: to.clone(),
                force: *force,
                wait: *wait,
                idempotency_key: idempotency_key.clone(),
            },
        ),
        Command::Backfill {
            workflow,
            from,
            to,
            force,
            yes,
        } => client::backfill(&o, workflow, from, to, *force, *yes),
        Command::PartitionStatus { workflow, from, to } => {
            client::partition_status(&o, workflow, from.clone(), to.clone())
        }
        Command::Runs { workflow, limit } => client::runs(&o, workflow.clone(), *limit),
        Command::Show { run } => client::show(&o, run),
        Command::Cancel { run } => client::cancel(&o, run),
        Command::Approvals { all } => client::approvals(&o, *all),
        Command::Approve { id } => client::decide(&o, id, ogun_api::methods::Decision::Approve),
        Command::Deny { id } => client::decide(&o, id, ogun_api::methods::Decision::Deny),
        Command::Credentials(cmd) => client::credentials(&o, cmd),
        Command::Secrets(cmd) => client::secrets(&o, cmd),
        Command::Approvers(cmd) => client::approvers(&o, cmd),
        Command::Edges(cmd) => client::edges(&o, cmd),
        Command::Events { after, follow, run } => client::events(&o, *after, *follow, run.clone()),
        Command::Artifacts { run } => client::artifacts(&o, run),
        Command::Artifact { blake3, cat } => client::artifact(&o, blake3, *cat),
        Command::Validate { file } => {
            let report = load(file)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(report.is_valid());
            }
            print_diagnostics(file, &report);
            if let (Some(hash), Some(s)) = (&report.workflow_hash, &report.summary) {
                println!("{} v{}: valid, {} nodes", s.workflow, s.version, s.nodes);
                println!("  workflow hash  {hash}");
                println!(
                    "  models         {}",
                    if s.uses_models {
                        "used"
                    } else {
                        "not used; runs with no model access"
                    }
                );
                if !s.code_nodes.is_empty() {
                    println!(
                        "  user code      {} (approval bound to this hash)",
                        s.code_nodes.join(", ")
                    );
                }
                if !s.always_approve.is_empty() {
                    println!("  approve always {}", s.always_approve.join(", "));
                }
            }
            Ok(report.is_valid())
        }
        Command::Hash { file } => {
            let report = load(file)?;
            match &report.workflow_hash {
                Some(hash) if cli.json => {
                    println!("{}", serde_json::json!({ "workflow_hash": hash }))
                }
                Some(hash) => println!("{hash}"),
                None => print_diagnostics(file, &report),
            }
            Ok(report.is_valid())
        }
        Command::Partitions {
            file,
            from,
            to,
            store,
        } => {
            let report = load(file)?;
            let Some(wf) = report.workflow() else {
                print_diagnostics(file, &report);
                return Ok(false);
            };
            let Some(decl) = &wf.partitions else {
                println!("{} is not partitioned", wf.workflow);
                return Ok(true);
            };
            let now = zeus_types::Timestamp::now().unix_millis();
            let keys = match (from, to) {
                (Some(f), Some(t)) => ogun_sched::range(decl, f, t, now)?,
                _ => ogun_sched::complete(decl, now)?,
            };
            let done = match store {
                Some(path) => {
                    let hash = report
                        .workflow_hash
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    ogun_store::Store::open(path)?.done_partitions(&hash)?
                }
                None => std::collections::BTreeSet::new(),
            };
            let plan = ogun_sched::plan(&keys, &done);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
                return Ok(true);
            }
            let span = |v: &[String]| match (v.first(), v.last()) {
                (Some(a), Some(b)) if a == b => a.clone(),
                (Some(a), Some(b)) => format!("{a} .. {b}"),
                _ => "none".to_owned(),
            };
            println!("{} partitions complete: {}", keys.len(), span(&keys));
            println!(
                "  to run      {} ({})",
                plan.to_run.len(),
                span(&plan.to_run)
            );
            println!("  up to date  {}", plan.up_to_date.len());
            Ok(true)
        }
        Command::Next { file } => {
            let report = load(file)?;
            let Some(wf) = report.workflow() else {
                print_diagnostics(file, &report);
                return Ok(false);
            };
            let now = zeus_types::Timestamp::now().unix_millis();
            match ogun_sched::next_fire(&wf.triggers, now, None)
                .and_then(|ms| zeus_types::Timestamp::from_unix_millis(ms).ok())
            {
                Some(t) => println!("{}", t.to_rfc3339()),
                None => println!("manual only"),
            }
            Ok(true)
        }
        Command::HashFile { file } => {
            let bytes =
                std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
            let hex = blake3::hash(&bytes).to_hex();
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "blake3": hex.as_str(), "size_bytes": bytes.len() })
                );
            } else {
                println!("{hex}");
            }
            Ok(true)
        }
        Command::Mcp => {
            let stdin = std::io::stdin();
            mcp::serve(stdin.lock(), std::io::stdout().lock(), || {
                zeus_types::Timestamp::now().unix_millis()
            })?;
            Ok(true)
        }
        Command::Diff { old, new } => {
            let (a, b) = (load(old)?, load(new)?);
            let (Some(wa), Some(wb)) = (a.workflow(), b.workflow()) else {
                print_diagnostics(old, &a);
                print_diagnostics(new, &b);
                return Ok(false);
            };
            let d = diff(wa, wb, &Catalogue::builtin());
            if cli.json {
                let out = serde_json::json!({
                    "from_hash": a.workflow_hash,
                    "to_hash": b.workflow_hash,
                    "diff": d,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
                return Ok(!d.needs_approval());
            }
            let (ha, hb) = (
                a.workflow_hash.as_ref().map(|h| h.short()),
                b.workflow_hash.as_ref().map(|h| h.short()),
            );
            println!(
                "v{} -> v{}   {} -> {}",
                wa.version,
                wb.version,
                ha.unwrap_or_default(),
                hb.unwrap_or_default()
            );
            if d.is_empty() {
                println!("  no change in nodes, budgets, triggers or partitions");
            }
            for id in &d.nodes_added {
                println!("  + {id}");
            }
            for id in &d.nodes_removed {
                println!("  - {id}");
            }
            for c in &d.nodes_changed {
                println!("  ~ {}  ({})", c.id, c.members.join(", "));
            }
            let show = |v: Option<u64>| v.map_or_else(|| "unlimited".to_owned(), |n| n.to_string());
            for b in &d.budgets {
                println!("  budget {}  {} -> {}", b.path, show(b.from), show(b.to));
            }
            if d.triggers_changed {
                println!("  triggers changed");
            }
            if d.partitions_changed {
                println!("  partitions changed");
            }
            if d.needs_approval() {
                println!("  needs approval:");
                for r in &d.approval_reasons {
                    println!("    {}", serde_json::to_string(r)?);
                }
            } else {
                println!("  runs under the existing approval");
            }
            Ok(!d.needs_approval())
        }
    }
}
