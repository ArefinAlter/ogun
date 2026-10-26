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
    /// Run for a supervising app (Ogun Studio): read its per-launch `studio` credential from the first line
    /// of stdin (never from disk), print one JSON line on stdout when listening, and stop when stdin closes.
    #[arg(long)]
    supervised: bool,
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
    let options = if args.supervised {
        ogun_daemon::StartOptions {
            transient_credential: Some(read_supervisor_credential().await?),
        }
    } else {
        ogun_daemon::StartOptions::default()
    };
    let endpoint = paths.endpoint().to_string();
    let running = ogun_daemon::start_with(paths, options)
        .await
        .context("ogund could not start")?;
    if args.supervised {
        // The one machine-readable line: the supervisor waits for it.
        #[allow(clippy::print_stdout)]
        {
            println!(
                "{}",
                serde_json::json!({
                    "event": "ready",
                    "version": env!("CARGO_PKG_VERSION"),
                    "endpoint": endpoint,
                    "pid": std::process::id(),
                })
            );
        }
        let daemon = running.daemon.clone();
        // The supervisor holds the write end of our stdin. When it closes (the app quit or was killed), stop
        // cleanly so in-flight attempts checkpoint and nothing runs unattended without a way to talk to it.
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt as _;
            let mut stdin = tokio::io::stdin();
            let mut sink = [0u8; 256];
            while matches!(stdin.read(&mut sink).await, Ok(n) if n > 0) {}
            tracing::info!("the supervisor closed stdin; stopping");
            daemon.shutdown.send_replace(true);
        });
    }
    tokio::select! {
        () = running.wait() => {}
        r = tokio::signal::ctrl_c() => { r.context("signal handler")?; }
        r = terminate() => { r.context("signal handler")?; }
    }
    tracing::info!("ogund stopping");
    running.stop().await;
    if args.supervised {
        // A blocking read of stdin may still be pending; do not wait for it.
        std::process::exit(0);
    }
    Ok(())
}

/// Reads the supervisor's `studio` credential: one line, `<id> human studio <token>` (the token-file format).
async fn read_supervisor_credential() -> anyhow::Result<ogun_api::Credential> {
    use tokio::io::AsyncBufReadExt as _;
    let mut line = zeroize::Zeroizing::new(String::new());
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        reader.read_line(&mut line),
    )
    .await
    .context("the supervisor sent no credential within 20 seconds")?
    .context("cannot read the supervisor's credential from stdin")?;
    ogun_api::Credential::from_file_text(&line).context("the supervisor's credential is malformed")
}

/// Resolves on SIGTERM (systemd, `docker stop`, Kubernetes); never on platforms without it.
async fn terminate() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?
            .recv()
            .await;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
        Ok(())
    }
}
