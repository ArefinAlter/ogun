//! `ogun`, the Ogun command-line interface.
//!
//! Phase O1 starts with the commands that need no daemon: `validate`, `hash` and `diff` read
//! workflow IR files and print what the runtime would accept, the hash an approval would bind to,
//! and what changed between two versions. Exit status: 0 valid, 1 invalid or approval needed
//! (`diff`), 2 usage or I/O error.

#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

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
    /// Show what changed between two versions and whether the new one needs an approval.
    Diff {
        /// The approved version.
        old: PathBuf,
        /// The proposed version.
        new: PathBuf,
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
    match &cli.command {
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
