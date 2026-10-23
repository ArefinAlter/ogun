//! The `ogun-server` binary: `serve`, `init`, `invite` and `check-config` (O-13 section 10).

#![forbid(unsafe_code)]

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use ogun_server::config::ServerConfig;
use ogun_server::store::{Role, Store};

/// Ogun's self-hosted HTTPS bridge to ogund.
#[derive(Parser)]
#[command(name = "ogun-server", version, about)]
struct Cli {
    /// `server.json`; default `$OGUN_SERVER_CONFIG`, else `/etc/ogun/server.json`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server.
    Serve {
        /// Plain HTTP behind a reverse proxy that terminates TLS (overrides `behind_proxy`).
        #[arg(long)]
        behind_proxy: bool,
        /// Listen address (overrides `listen`).
        #[arg(long)]
        listen: Option<String>,
    },
    /// Check `server.json` and print every finding; exit 1 on errors.
    CheckConfig {
        /// Print the findings as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create the first workspace and print a one-time owner enrolment link. Run it while the server is stopped.
    Init {
        /// Workspace name.
        #[arg(long, default_value = "Main")]
        name: String,
        /// The `daemons` entry the workspace uses.
        #[arg(long, default_value = "default")]
        daemon: String,
    },
    /// Print a one-time enrolment link for an existing workspace (host-side recovery). Run it while the server is
    /// stopped.
    Invite {
        /// The workspace, `wsp_...`.
        #[arg(long)]
        workspace: String,
        /// Role: viewer, editor, admin or owner.
        #[arg(long)]
        role: String,
        /// Also grant the approver flag.
        #[arg(long)]
        approver: bool,
    },
}

fn config_path(cli: Option<PathBuf>) -> PathBuf {
    cli.or_else(|| std::env::var_os("OGUN_SERVER_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/etc/ogun/server.json"))
}

fn load(path: &std::path::Path) -> anyhow::Result<ServerConfig> {
    ServerConfig::load(path).map_err(|e| anyhow::anyhow!(e.envelope.message))
}

fn out(line: &str) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{line}");
}

fn check(cfg: &ServerConfig, json: bool) -> anyhow::Result<bool> {
    let findings = cfg.check();
    if json {
        out(&serde_json::to_string_pretty(&findings)?);
    } else if findings.is_empty() {
        out("ok: no findings");
    } else {
        for f in &findings {
            out(&format!("{}: {}", f.level, f.message));
        }
    }
    Ok(!findings.iter().any(|f| f.level == "error"))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}

async fn serve(
    mut cfg: ServerConfig,
    behind_proxy: bool,
    listen: Option<String>,
) -> anyhow::Result<()> {
    if behind_proxy {
        cfg.behind_proxy = true;
    }
    if let Some(l) = listen {
        cfg.listen = l;
    }
    let findings = cfg.check();
    for f in &findings {
        if f.level == "error" {
            tracing::error!("{}", f.message);
        } else {
            tracing::warn!("{}", f.message);
        }
    }
    if findings.iter().any(|f| f.level == "error") {
        bail!("server.json has errors; run `ogun-server check-config`");
    }
    if cfg.remote_approvals {
        tracing::warn!(
            "remote_approvals is ON. ADR 0010 requires an external security review of remote approvals before release; do not rely on this build for remote approvals on an untrusted network"
        );
    }
    let state = ogun_server::build_state(cfg).map_err(|e| anyhow::anyhow!(e.envelope.message))?;
    for (name, pool) in &state.daemons {
        match pool.probe().await {
            Ok(s) => {
                tracing::info!(daemon = %name, version = %s.daemon_version, "connected to ogund")
            }
            Err(e) => {
                tracing::warn!(daemon = %name, code = %e.code(), message = %e.envelope.message, "ogund is not reachable yet; requests will retry")
            }
        }
    }
    match &state.studio_dir {
        Some(d) => tracing::info!(dir = %d.display(), "serving Studio-web"),
        None => tracing::info!("Studio-web build not found; serving the built-in page"),
    }
    let purge_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(600));
        loop {
            tick.tick().await;
            let s = &purge_state.cfg.sessions;
            let idle = Duration::from_secs(s.idle_timeout_minutes * 60);
            let abs = Duration::from_secs(s.absolute_timeout_hours * 3600);
            if let Ok(n) = purge_state.store.purge(idle, abs)
                && n > 0
            {
                tracing::debug!(
                    removed = n,
                    "expired sessions, sign-ins and invitations removed"
                );
            }
        }
    });
    ogun_server::serve::run(state, shutdown_signal())
        .await
        .map_err(|e| anyhow::anyhow!(e.envelope.message))
}

fn open_store(cfg: &ServerConfig) -> anyhow::Result<Store> {
    let dir = cfg
        .state_dir()
        .map_err(|e| anyhow::anyhow!(e.envelope.message))?;
    Store::open(&dir).map_err(|e| anyhow::anyhow!(e.envelope.message))
}

fn init(cfg: &ServerConfig, name: &str, daemon: &str) -> anyhow::Result<()> {
    if !cfg.daemons.contains_key(daemon) {
        bail!("server.json has no daemon named {daemon}");
    }
    let derived = cfg
        .derive()
        .map_err(|e| anyhow::anyhow!(e.envelope.message))?;
    let store = open_store(cfg)?;
    if !store
        .workspaces()
        .map_err(|e| anyhow::anyhow!(e.envelope.message))?
        .is_empty()
    {
        bail!(
            "this server already has a workspace; use `ogun-server invite` for another enrolment link"
        );
    }
    let w = store
        .create_workspace(name, daemon)
        .map_err(|e| anyhow::anyhow!(e.envelope.message))?;
    let (url, expires) = ogun_server::routes::admin::create_invite(
        &store,
        &derived.origin,
        cfg.invites.ttl_hours,
        w.workspace_id,
        Role::Owner,
        true,
        None,
    )
    .map_err(|e| anyhow::anyhow!(e.envelope.message))?;
    out(&format!("workspace: {} ({})", w.workspace_id, w.name));
    out(&format!(
        "owner enrolment link (works once, until {}):",
        expires.to_rfc3339()
    ));
    out(&url);
    out("Open it in a browser and sign in with your identity provider to become the owner.");
    Ok(())
}

fn invite(cfg: &ServerConfig, workspace: &str, role: &str, approver: bool) -> anyhow::Result<()> {
    let derived = cfg
        .derive()
        .map_err(|e| anyhow::anyhow!(e.envelope.message))?;
    let role: Role = role
        .parse()
        .map_err(|e: ogun_server::error::ServerError| anyhow::anyhow!(e.envelope.message))?;
    let w = workspace.parse().context("workspace must be a wsp_ id")?;
    let store = open_store(cfg)?;
    if store
        .workspace(w)
        .map_err(|e| anyhow::anyhow!(e.envelope.message))?
        .is_none()
    {
        bail!("no such workspace");
    }
    let (url, expires) = ogun_server::routes::admin::create_invite(
        &store,
        &derived.origin,
        cfg.invites.ttl_hours,
        w,
        role,
        approver,
        None,
    )
    .map_err(|e| anyhow::anyhow!(e.envelope.message))?;
    out(&format!(
        "enrolment link for {workspace} as {} (works once, until {}):",
        role.as_str(),
        expires.to_rfc3339()
    ));
    out(&url);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // One process-wide TLS provider for the OIDC client (the server side names its provider explicitly).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let path = config_path(cli.config);
    let cfg = load(&path)?;
    match cli.command {
        Command::CheckConfig { json } => {
            if !check(&cfg, json)? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Init { name, daemon } => init(&cfg, &name, &daemon),
        Command::Invite {
            workspace,
            role,
            approver,
        } => invite(&cfg, &workspace, &role, approver),
        Command::Serve {
            behind_proxy,
            listen,
        } => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(serve(cfg, behind_proxy, listen)),
    }
}
