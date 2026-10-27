//! The methods behind Ogun Studio's settings and agent screens (O-16 sections 5 and 6, O-17 section 5).
//!
//! Everything here is the daemon's own state, stored as small JSON documents under `<state>/settings/`, or in the
//! run store and artifact store the daemon already keeps. None of it can decide an approval, read a secret value,
//! or run code: connection tests and agent tools reach only what their own module says, and the policy table
//! (`ogun_api::policy`) fixes which kinds of client may call each method.

pub mod agent;
pub mod bench;
pub mod connections;
pub mod environments;
pub mod ladder;
pub mod runtime;
pub mod storage;

use std::path::PathBuf;
use std::sync::Arc;

use ogun_api::{ApiError, ApiResult};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::creds::{private_dir, write_private};
use crate::server::Conn;
use crate::state::Daemon;

/// Small JSON documents, one file each, replaced atomically (`0600` on Unix).
#[derive(Debug, Clone)]
pub struct Docs {
    dir: PathBuf,
}

impl Docs {
    /// The `settings` directory of a state directory, created.
    ///
    /// # Errors
    ///
    /// `api.unavailable`.
    pub fn open(state_dir: &std::path::Path) -> ApiResult<Self> {
        let dir = state_dir.join("settings");
        private_dir(&dir)?;
        Ok(Self { dir })
    }

    /// The directory.
    #[must_use]
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Reads a document; the default when it does not exist or does not parse.
    #[must_use]
    pub fn read<T: DeserializeOwned + Default>(&self, name: &str) -> T {
        std::fs::read(self.dir.join(format!("{name}.json")))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Writes a document.
    ///
    /// # Errors
    ///
    /// `api.unavailable`.
    pub fn write<T: Serialize>(&self, name: &str, value: &T) -> ApiResult<()> {
        let bytes =
            serde_json::to_vec_pretty(value).map_err(|e| ApiError::internal(e.to_string()))?;
        write_private(&self.dir.join(format!("{name}.json")), &bytes)
    }
}

/// What the daemon keeps for these methods, owned by [`Daemon`].
pub struct Studio {
    /// The document store.
    pub docs: Docs,
    /// Run defaults and per-workflow overrides.
    pub runtime: runtime::RuntimeState,
    /// Data connections.
    pub connections: Arc<connections::Connections>,
    /// The agent: threads, settings and the spend ledger.
    pub agent: agent::Agents,
    /// Benchmark runs started from the app.
    pub bench: bench::Benches,
}

impl std::fmt::Debug for Studio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Studio").finish_non_exhaustive()
    }
}

impl Studio {
    /// Opens the documents under `state_dir`.
    ///
    /// # Errors
    ///
    /// `api.unavailable`.
    pub fn open(
        state_dir: &std::path::Path,
        docs: Docs,
        connections: Arc<connections::Connections>,
        scheduler_default: bool,
        parallel_default: u32,
    ) -> ApiResult<Self> {
        Ok(Self {
            runtime: runtime::RuntimeState::load(&docs, scheduler_default, parallel_default),
            connections,
            agent: agent::Agents::open(state_dir, &docs)?,
            bench: bench::Benches::new(state_dir),
            docs,
        })
    }
}

/// Parses parameters, treating `null` as `{}`.
///
/// # Errors
///
/// `api.invalid_params`.
pub fn args<T: DeserializeOwned>(v: Value) -> ApiResult<T> {
    crate::server::params(v)
}

/// Serialises a result.
///
/// # Errors
///
/// `internal.bug`.
pub fn out<T: Serialize>(v: &T) -> ApiResult<Value> {
    crate::server::result(v)
}

/// Routes the methods of this module. `None` means the method is not one of them.
pub async fn dispatch(
    d: &Arc<Daemon>,
    conn: &Conn,
    method: &str,
    p: Value,
) -> Option<ApiResult<Value>> {
    let prefix = method.split('.').next().unwrap_or("");
    match prefix {
        "agent" => agent::dispatch(d, conn, method, p).await,
        "connections" => connections::dispatch(d, conn, method, p).await,
        "runtime" | "scheduler" => runtime::dispatch(d, conn, method, p),
        "environments" | "sandbox" | "evidence" => environments::dispatch(d, conn, method, p).await,
        "storage" => storage::dispatch(d, conn, method, p),
        "ladder" => ladder::dispatch(d, conn, method, p),
        "bench" => bench::dispatch(d, conn, method, p).await,
        _ => None,
    }
}
