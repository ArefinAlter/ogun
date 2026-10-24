//! `ogun-bench`: run benchmark matrices in isolated environments (O-06).
//!
//! ```text
//! ogun-bench run --matrix matrix.json --out results/      run agents x tasks x seeds
//! ogun-bench tasks --adapter harbor --path tasks/           list what an adapter reads
//! ogun-bench report --results results/                      print results.md again from results.json
//! ogun-bench replay results/runs/agr_.../trajectory.jsonl   verify and print a trajectory
//! ogun-bench exec --env-dir <dir> -- <argv>                 run a command in an environment (host agents)
//! ```
//!
//! The binary never reads Ogun's or Zeus's state and never talks to `ogund` or `zeusd`.

#![forbid(unsafe_code)]
// A command-line program writes its results to stdout and its diagnostics to stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ogun_bench::adapters::{self, BenchmarkSource};
use ogun_bench::env::{Environment, ExecOptions, exec_in};
use ogun_bench::matrix::{MatrixResults, MatrixSpec, render_markdown, run_matrix};
use ogun_lineage::{TrajectoryEvent, TrajectoryKind, verify_trajectory};

#[derive(Parser)]
#[command(
    name = "ogun-bench",
    version,
    about = "Run agent benchmarks in isolated environments (Ogun O-06)"
)]
struct Cli {
    /// The benchmark state directory (environments, caches). Never inside Ogun's or Zeus's state.
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a matrix and write its results directory.
    Run {
        /// The matrix specification (JSON).
        #[arg(long)]
        matrix: PathBuf,
        /// The results directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// List the tasks an adapter reads from a path.
    Tasks {
        /// `harbor`, `spider2_dbt` or `ade_bench`.
        #[arg(long)]
        adapter: String,
        /// The benchmark directory.
        #[arg(long)]
        path: PathBuf,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print the Markdown results table of a results directory.
    Report {
        /// The results directory.
        #[arg(long)]
        results: PathBuf,
    },
    /// Verify a trajectory's chain and print it.
    Replay {
        /// A `trajectory.jsonl`.
        trajectory: PathBuf,
        /// Print only the summary.
        #[arg(long)]
        summary: bool,
    },
    /// Run a command in an existing environment (the shim host-placed agents use).
    Exec {
        /// The environment directory (`$OGUN_BENCH_ENV_DIR`).
        #[arg(long)]
        env_dir: PathBuf,
        /// Time limit.
        #[arg(long, default_value_t = 600_000)]
        timeout_ms: u64,
        /// The command.
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
}

fn default_state_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("OGUN_BENCH_STATE_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let base = directories::BaseDirs::new().context("cannot determine the home directory")?;
    Ok(if cfg!(windows) || cfg!(target_os = "macos") {
        base.data_local_dir().join("OgunBench")
    } else {
        base.state_dir()
            .unwrap_or_else(|| base.data_local_dir())
            .join("ogun-bench")
    })
}

fn describe(kind: &TrajectoryKind) -> String {
    let v = serde_json::to_value(kind).unwrap_or_default();
    let ty = v
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("?")
        .to_owned();
    let mut rest = v.as_object().cloned().unwrap_or_default();
    rest.remove("type");
    let mut text = serde_json::to_string(&rest).unwrap_or_default();
    if text.len() > 200 {
        let mut end = 200;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    format!("{ty} {text}")
}

fn replay(path: &Path, summary_only: bool) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let summary =
        verify_trajectory(bytes.as_slice()).map_err(|e| anyhow::anyhow!("{}: {e}", e.code()))?;
    let mut out = std::io::stdout().lock();
    if !summary_only {
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let e: TrajectoryEvent = serde_json::from_slice(line)?;
            writeln!(
                out,
                "{:>5} {:>9} ms  {}",
                e.seq,
                e.elapsed_ms,
                describe(&e.kind)
            )?;
        }
    }
    writeln!(out, "{}", serde_json::to_string_pretty(&summary)?)?;
    Ok(())
}

async fn exec(env_dir: &Path, timeout_ms: u64, argv: &[String]) -> Result<ExitCode> {
    let record =
        Environment::load_record(env_dir).map_err(|e| anyhow::anyhow!("{}: {e}", e.code()))?;
    let started = std::time::Instant::now();
    let out = exec_in(
        &record,
        argv,
        &ExecOptions {
            timeout_ms,
            ..ExecOptions::default()
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("{}: {e}", e.code()))?;
    // The command and its result go to the agent's own log, which the harness ingests after the run.
    let log = record.logs.join("agent").join("ogun-trajectory.jsonl");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    {
        let call = TrajectoryKind::ToolShell {
            untrusted_command: argv.join(" "),
        };
        let result = TrajectoryKind::ToolResult {
            tool: "shell".into(),
            exit_code: out.exit_code,
            duration_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            untrusted_output: None,
        };
        for k in [call, result] {
            if let Ok(line) = serde_json::to_string(&k) {
                let _ = writeln!(f, "{line}");
            }
        }
    }
    std::io::stdout().lock().write_all(&out.stdout)?;
    std::io::stderr().lock().write_all(&out.stderr)?;
    if out.timed_out {
        eprintln!("ogun-bench exec: the command exceeded {timeout_ms} ms and was stopped");
        return Ok(ExitCode::from(124));
    }
    Ok(match out.exit_code {
        Some(0) => ExitCode::SUCCESS,
        Some(c) => ExitCode::from(u8::try_from(c.clamp(1, 255)).unwrap_or(1)),
        None => ExitCode::from(137),
    })
}

async fn real_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { matrix, out } => {
            let text =
                std::fs::read(&matrix).with_context(|| format!("reading {}", matrix.display()))?;
            let spec: MatrixSpec =
                serde_json::from_slice(&text).context("the matrix specification is invalid")?;
            let state = match cli.state_dir {
                Some(s) => s,
                None => default_state_dir()?,
            };
            let results = run_matrix(&spec, &state, &out)
                .await
                .map_err(|e| anyhow::anyhow!("{}: {e}", e.code()))?;
            print!("{}", render_markdown(&results));
            Ok(ExitCode::SUCCESS)
        }
        Command::Tasks {
            adapter,
            path,
            json,
        } => {
            let src = BenchmarkSource {
                adapter,
                path,
                tasks: vec![],
                limit: None,
                evaluator_command: None,
                evaluator_result: None,
                agent_timeout_ms: None,
            };
            let tasks = adapters::load(&src).map_err(|e| anyhow::anyhow!("{}: {e}", e.code()))?;
            let mut out = std::io::stdout().lock();
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&tasks)?)?;
            } else {
                for t in &tasks {
                    writeln!(
                        out,
                        "{}\t{}\t{}\tagent {} ms\t{}",
                        t.benchmark,
                        t.id,
                        &t.task_blake3[..16],
                        t.agent_timeout_ms,
                        t.evaluator.label()
                    )?;
                }
                writeln!(out, "{} tasks", tasks.len())?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Report { results } => {
            let bytes =
                std::fs::read(results.join("results.json")).context("reading results.json")?;
            let r: MatrixResults =
                serde_json::from_slice(&bytes).context("results.json is invalid")?;
            print!("{}", render_markdown(&r));
            Ok(ExitCode::SUCCESS)
        }
        Command::Replay {
            trajectory,
            summary,
        } => {
            replay(&trajectory, summary)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Exec {
            env_dir,
            timeout_ms,
            argv,
        } => {
            if argv.is_empty() {
                bail!("no command");
            }
            exec(&env_dir, timeout_ms, &argv).await
        }
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("OGUN_BENCH_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("ogun-bench: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(real_main()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ogun-bench: {e:#}");
            ExitCode::FAILURE
        }
    }
}
