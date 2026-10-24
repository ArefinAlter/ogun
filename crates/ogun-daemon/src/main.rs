//! The `ogund` binary.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use ogun_api::OgunPaths;

/// Ogun's runtime daemon. One per user and profile; listens only on a local IPC endpoint.
#[derive(Debug, Parser)]
#[command(name = "ogund", version, about)]
struct Args {
    /// Use a separate profile (its own state and endpoint).
    #[arg(long)]
    profile: Option<String>,
    /// State directory (overrides OGUN_HOME and the platform default).
    #[arg(long)]
    home: Option<PathBuf>,
    /// Print the endpoint and state directory, then exit.
    #[arg(long)]
    print_paths: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("OGUN_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let paths = match &args.home {
        Some(h) => OgunPaths::rooted(h, args.profile.as_deref()),
        None => OgunPaths::discover(args.profile.as_deref())
            .context("cannot find the state directory")?,
    };
    if args.print_paths {
        #[allow(clippy::print_stdout)]
        {
            println!("state    {}", paths.state_dir.display());
            println!("endpoint {}", paths.endpoint());
        }
        return Ok(());
    }
    let running = ogun_daemon::start(paths)
        .await
        .context("ogund could not start")?;
    tokio::select! {
        () = running.wait() => {}
        r = tokio::signal::ctrl_c() => { r.context("signal handler")?; }
    }
    tracing::info!("ogund stopping");
    running.stop().await;
    Ok(())
}
