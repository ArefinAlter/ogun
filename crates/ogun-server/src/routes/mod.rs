//! The HTTP surface (O-13 section 6): REST under `/api/v1`, sign-in under `/auth`, enrolment links, MCP at
//! `/mcp`, and Studio-web for everything else.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, patch, post};
use tokio::sync::Semaphore;

use crate::config::{Derived, ServerConfig};
use crate::daemon::DaemonPool;
use crate::error::{ServerError, ServerResult};
use crate::oidc::Oidc;
use crate::ratelimit::Limiter;
use crate::store::{Store, Workspace};

pub mod admin;
pub mod api;
pub mod approvals;
pub mod artifacts;
pub mod auth;
pub mod events;
pub mod mcp;
pub mod studio;
pub mod sync;

/// Everything a handler needs.
#[derive(Debug)]
pub struct AppState {
    /// The configuration.
    pub cfg: ServerConfig,
    /// Facts derived from it.
    pub derived: Derived,
    /// The server store.
    pub store: Store,
    /// Daemon pools by name.
    pub daemons: HashMap<String, Arc<DaemonPool>>,
    /// OIDC.
    pub oidc: Oidc,
    /// Rate limits.
    pub limiter: Limiter,
    /// Open event streams.
    pub sse: Arc<Semaphore>,
    /// The Studio-web build, when found.
    pub studio_dir: Option<PathBuf>,
}

impl AppState {
    /// The pool of a workspace's daemon.
    ///
    /// # Errors
    ///
    /// `api.unavailable` when the workspace names a daemon missing from `server.json`.
    pub fn pool(&self, w: &Workspace) -> ServerResult<Arc<DaemonPool>> {
        self.daemons.get(&w.daemon).cloned().ok_or_else(|| {
            ServerError::unavailable(format!(
                "workspace {} uses daemon {}, which server.json does not configure",
                w.workspace_id, w.daemon
            ))
        })
    }
}

/// The application router.
pub fn router(state: Arc<AppState>) -> Router {
    let max_body = state.cfg.limits.max_body_bytes;
    let api = Router::new()
        .route("/status", get(api::status))
        .route("/me", get(api::me))
        .route(
            "/workflows",
            get(api::workflows_list).post(api::workflows_put),
        )
        .route("/workflows/validate", post(api::workflows_validate))
        .route("/workflows/{name}", get(api::workflows_get))
        .route(
            "/workflows/{name}/enabled",
            post(api::workflows_set_enabled),
        )
        .route("/workflows/{name}/runs", post(api::runs_start))
        .route("/workflows/{name}/partitions", get(api::partitions_status))
        .route("/workflows/{name}/backfill-plan", post(api::backfill_plan))
        .route("/runs", get(api::runs_list))
        .route("/runs/{run_id}", get(api::runs_get))
        .route("/runs/{run_id}/cancel", post(api::runs_cancel))
        .route("/events", get(events::stream))
        .route("/approvals", get(api::approvals_list))
        .route("/approvals/{approval_id}", get(api::approvals_get))
        .route(
            "/approvals/{approval_id}/webauthn-options",
            post(approvals::decision_options),
        )
        .route("/approvals/{approval_id}/decide", post(approvals::decide))
        .route("/artifacts", get(api::artifacts_list))
        .route("/artifacts/{blake3}", get(api::artifacts_get))
        .route("/artifacts/{blake3}/content", get(artifacts::content))
        .route("/lineage", get(api::lineage))
        .route("/streams", get(api::streams_list))
        .route("/streams/{workflow}/{node}", get(api::streams_status))
        .route("/edges", get(api::edges_list))
        .route("/passkeys", get(approvals::passkeys_list))
        .route(
            "/passkeys/register-options",
            post(approvals::register_options),
        )
        .route("/passkeys/register", post(approvals::register))
        .route("/sync/offer", post(sync::offer))
        .route("/sync/chunk", post(sync::chunk))
        .route("/workspaces", get(admin::workspaces))
        .route("/workspaces/{workspace_id}/members", get(admin::members))
        .route(
            "/workspaces/{workspace_id}/members/{user_id}",
            patch(admin::member_update).delete(admin::member_remove),
        )
        .route(
            "/workspaces/{workspace_id}/invites",
            post(admin::invite_create),
        )
        .route("/tokens", get(admin::tokens_list).post(admin::token_create))
        .route("/tokens/{token_id}", delete(admin::token_revoke))
        .fallback(api::not_found);
    let auth = Router::new()
        .route("/providers", get(auth::providers))
        .route("/login", get(auth::login))
        .route("/callback", get(auth::callback))
        .route("/native", post(auth::native))
        .route("/logout", post(auth::logout));
    let mut app = Router::new()
        .nest("/api/v1", api)
        .nest("/auth", auth)
        .route("/enroll/{code}", get(auth::enroll));
    if state.cfg.mcp.enabled {
        app = app.route(
            "/mcp",
            post(mcp::post)
                .get(mcp::not_allowed)
                .delete(mcp::not_allowed),
        );
    }
    let app = app.route("/_ogun/style.css", get(studio::style));
    let app = match state.studio_dir.clone() {
        Some(dir) => app.fallback_service(studio::service(dir)),
        None => app.fallback(studio::builtin),
    };
    app.layer(DefaultBodyLimit::max(max_body))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            crate::security::guard,
        ))
        .with_state(state)
}
