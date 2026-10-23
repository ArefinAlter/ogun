//! `ogun-ingest`: the Ogun ingest bridge (Ogun spec O-10; ADRs 0007 and 0008).
//!
//! A separate process from `ogund` (one listener per daemon). It runs the `source` operators of
//! every enabled stream workflow: `stream.mqtt`, `stream.http`, `stream.lorawan`, `stream.modbus`
//! and `stream.file_tail`. Each source appends to its own durable ingest log first and
//! acknowledges the device or broker only after the append commits; a forwarder then pushes the
//! records to `ogund` with `streams.push` and trims the log as the daemon accepts them.
//!
//! It connects to `ogund` with the `service` credential `credentials/ingest.token` and reads which
//! sources exist from `workflows.list` and `workflows.get`. Its own settings (bind addresses,
//! broker overrides, secret files) are in `<state_dir>/ingest.json`.

#![forbid(unsafe_code)]

mod applog;
mod config;
mod decode;
mod forward;
mod sources;
mod spec;
mod status;
mod supervisor;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use ogun_api::OgunPaths;

/// Command line.
#[derive(Debug, Parser)]
#[command(
    name = "ogun-ingest",
    version,
    about = "The Ogun ingest bridge: MQTT, HTTP push, LoRaWAN, Modbus and file sources (O-10)"
)]
struct Cli {
    /// Ogun profile (a separate daemon and state directory).
    #[arg(long)]
    profile: Option<String>,
    /// Configuration file; `<state_dir>/ingest.json` by default.
    #[arg(long)]
    config: Option<PathBuf>,
    /// The `ogund` credential (kind `service`) under `<state_dir>/credentials/`.
    #[arg(long, default_value = "ingest")]
    credential: String,
}

fn init_logs() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("OGUN_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logs();
    let paths = OgunPaths::discover(cli.profile.as_deref()).context("no Ogun state directory")?;
    let config = cli
        .config
        .unwrap_or_else(|| paths.state_dir.join("ingest.json"));
    tracing::info!(state_dir = %paths.state_dir.display(), "ogun-ingest starting");
    let mut supervisor = supervisor::Supervisor::new(paths, config, cli.credential);
    tokio::select! {
        () = supervisor.run() => {}
        r = tokio::signal::ctrl_c() => {
            r.context("cannot wait for ctrl-c")?;
            tracing::info!("stopping; records stay in the bridge logs");
        }
    }
    supervisor.shutdown().await;
    Ok(())
}
