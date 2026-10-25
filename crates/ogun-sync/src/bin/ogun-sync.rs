//! `ogun-sync`: the edge-to-hub sync client (Ogun spec O-11).
//!
//! * `ogun-sync keygen` creates the edge's Ed25519 key (`<state_dir>/sync/edge.key`, `0600`) and
//!   prints its public key for a person to enrol on the hub (`ogun edges enroll`).
//! * `ogun-sync once` runs one sync round and prints its report as JSON.
//! * `ogun-sync run` runs rounds forever, `interval_ms` apart.
//!
//! Configuration is `<state_dir>/sync.json` (see `ogun_sync::edge::SyncConfig`).

#![forbid(unsafe_code)]

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ogun_api::OgunPaths;
use ogun_sync::edge::{EdgeSync, SyncConfig};
use ogun_sync::keys::EdgeKey;

#[derive(Debug, Parser)]
#[command(
    name = "ogun-sync",
    version,
    about = "Edge-to-hub sync for Ogun (O-11)"
)]
struct Cli {
    /// Ogun profile (a separate daemon and state directory).
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Configuration file; `<state_dir>/sync.json` by default.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the edge key and print its public key (base64url) for enrolment on the hub.
    Keygen {
        /// Where to write the key; `<state_dir>/sync/edge.key` by default.
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    /// Run one sync round and print its report.
    Once,
    /// Run sync rounds forever.
    Run {
        /// Override `interval_ms` from the configuration.
        #[arg(long)]
        interval_ms: Option<u64>,
    },
}

fn init_logs() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("OGUN_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn print_line(text: &str) -> Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{text}").context("cannot write to stdout")
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logs();
    let paths = OgunPaths::discover(cli.profile.as_deref()).context("no Ogun state directory")?;
    match cli.command {
        Command::Keygen { key_file } => {
            let path = key_file.unwrap_or_else(|| SyncConfig::default_key_file(&paths));
            let key = EdgeKey::generate()?;
            key.write_new(&path)
                .with_context(|| format!("cannot create {}", path.display()))?;
            print_line(&key.public_key())?;
            tracing::info!(
                key_file = %path.display(),
                "edge key created; enrol the public key on the hub, then put the edge id in sync.json"
            );
        }
        Command::Once => {
            let file = cli.config.unwrap_or_else(|| SyncConfig::file(&paths));
            let config = SyncConfig::load(&file)?;
            let mut sync = EdgeSync::new(config, &paths)?;
            let report = sync.once(&paths).await?;
            print_line(&serde_json::to_string(&report)?)?;
        }
        Command::Run { interval_ms } => {
            let file = cli.config.unwrap_or_else(|| SyncConfig::file(&paths));
            let mut config = SyncConfig::load(&file)?;
            if let Some(ms) = interval_ms {
                config.interval_ms = ms;
            }
            let mut sync = EdgeSync::new(config, &paths)?;
            tracing::info!(hub = %sync.hub_for_logs(), pending = sync.pending(), "ogun-sync running");
            tokio::select! {
                () = sync.run(&paths) => {}
                _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
            }
        }
    }
    Ok(())
}
