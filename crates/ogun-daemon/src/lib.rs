//! `ogund`, Ogun's runtime daemon.
//!
//! One process per user (and profile) with exactly one listener, the local IPC endpoint of O-15.
//! It owns the run store, the artifact store, the credential table and the secret store; it fires
//! triggers ([`sched`]), runs node instances ([`exec`]), seals stream windows ([`streams`]) and
//! keeps approvals ([`approvals`]). HTTP surfaces are separate bridge processes.

#![forbid(unsafe_code)]

pub mod approvals;
pub mod config;
pub mod creds;
pub mod error;
pub mod exec;
pub mod handlers;
pub mod lineage;
mod llm_budget;
pub mod sched;
pub mod secrets;
pub mod server;
pub mod session;
pub mod state;
pub mod streams;
pub mod sync;
pub mod webauthn;

use std::sync::Arc;

use ogun_api::OgunPaths;
use ogun_api::transport::Listener;

pub use error::{DaemonError, DaemonResult};
pub use state::Daemon;

/// A started daemon: its shared state and the tasks serving it.
#[derive(Debug)]
pub struct Running {
    /// Shared state.
    pub daemon: Arc<Daemon>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    /// Asks every task to stop and waits for them.
    pub async fn stop(self) {
        self.daemon.shutdown.send_replace(true);
        for t in self.tasks {
            let _ = t.await;
        }
    }

    /// Waits until something requests shutdown (`daemon.shutdown` or [`Running::stop`]).
    pub async fn wait(&self) {
        let mut rx = self.daemon.shutdown.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Opens the state, binds the endpoint and starts the server, executor, scheduler and streams.
///
/// # Errors
///
/// Any startup failure, including another `ogun` daemon already listening.
pub async fn start(paths: OgunPaths) -> DaemonResult<Running> {
    start_with(paths, StartOptions::default()).await
}

/// What a supervising app adds to a start.
#[derive(Debug, Default)]
pub struct StartOptions {
    /// A credential held in memory only (see [`creds::Credentials::add_transient`]).
    pub transient_credential: Option<ogun_api::Credential>,
}

/// As [`start`], with the options of a supervised launch.
///
/// # Errors
///
/// Any startup failure, including another daemon already listening.
pub async fn start_with(paths: OgunPaths, options: StartOptions) -> DaemonResult<Running> {
    let endpoint = paths.endpoint();
    let listener = Listener::bind(&endpoint).await?;
    let daemon = tokio::task::spawn_blocking(move || Daemon::open(paths))
        .await
        .map_err(|_| DaemonError::State("startup task failed".into()))??;
    if let Some(cred) = &options.transient_credential {
        state::lock(&daemon.creds).add_transient(cred)?;
    }
    tracing::info!(endpoint = %endpoint, state = %daemon.paths.state_dir.display(), "ogund listening");
    daemon.streams.activate_all(&daemon);
    let tasks = vec![
        tokio::spawn(server::serve(daemon.clone(), listener)),
        tokio::spawn(exec::run(daemon.clone())),
        tokio::spawn(sched::run(daemon.clone())),
        tokio::spawn(session::run(daemon.clone())),
    ];
    daemon.changed();
    Ok(Running { daemon, tasks })
}
