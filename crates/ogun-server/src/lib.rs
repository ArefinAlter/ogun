//! `ogun-server`: the self-hosted HTTPS bridge to `ogund` (Ogun spec O-13; ADR 0010).
//!
//! The daemon keeps its single local listener. This separate process owns every network surface: OIDC sign-in,
//! users, workspaces and roles, the Studio-web REST API and event stream, artifact downloads, MCP over streamable
//! HTTP for API tokens, and the edge sync relay. It reaches the daemon only as an `ogun_api::Client` with a
//! `bridge` credential, so a compromised bridge holds a bridge's authority and nothing more: it cannot confirm an
//! approver key, and it cannot decide an approval without a WebAuthn assertion that the daemon verifies itself.
//!
//! API tokens always act as agents. The bridge enforces the agent ceiling of the daemon's policy table for every
//! token request, and no route a token can reach decides an approval.

#![forbid(unsafe_code)]

pub mod config;
pub mod daemon;
pub mod error;
pub mod oidc;
pub mod principal;
pub mod ratelimit;
pub mod routes;
pub mod security;
pub mod serve;
pub mod store;
pub mod util;
pub mod webauthn;

use std::collections::HashMap;
use std::sync::Arc;

use config::ServerConfig;
use error::{ServerError, ServerResult};
use routes::AppState;

/// Builds the shared state from a checked configuration.
///
/// # Errors
///
/// Store, OIDC and configuration errors.
pub fn build_state(cfg: ServerConfig) -> ServerResult<Arc<AppState>> {
    let derived = cfg.derive()?;
    let store = store::Store::open(&cfg.state_dir()?)?;
    let mut daemons = HashMap::new();
    for (name, d) in &cfg.daemons {
        daemons.insert(
            name.clone(),
            daemon::DaemonPool::new(name, d.paths()?, &d.credential, d.pool_size),
        );
    }
    let oidc = oidc::Oidc::new(&cfg.oidc.providers)?;
    let l = &cfg.limits;
    let limiter = ratelimit::Limiter::new(
        l.per_ip_per_minute,
        l.auth_per_ip_per_minute,
        l.sync_per_ip_per_minute,
        l.per_token_per_minute,
    );
    let sse = Arc::new(tokio::sync::Semaphore::new(l.max_sse_streams.max(1)));
    let studio_dir = routes::studio::find_dir(cfg.studio_dir.as_deref());
    for w in store.workspaces()? {
        if !daemons.contains_key(&w.daemon) {
            return Err(ServerError::invalid(format!(
                "workspace {} uses daemon {}, which server.json does not configure",
                w.workspace_id, w.daemon
            )));
        }
    }
    Ok(Arc::new(AppState {
        cfg,
        derived,
        store,
        daemons,
        oidc,
        limiter,
        sse,
        studio_dir,
    }))
}
