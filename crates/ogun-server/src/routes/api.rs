//! The REST routes that map one to one onto daemon methods (O-13 section 6.2).
//!
//! Each handler authenticates, resolves the workspace, checks the role (and, for API tokens, that an agent may call
//! the method at all), then relays the call to the workspace's daemon with the `ogun_api::methods` parameter type.
//! Results are the daemon's results, unchanged, except that `artifacts.get` drops the daemon-local file path.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, Uri};
use axum::response::{IntoResponse, Response};
use ogun_api::methods::{
    ApprovalRef, ApprovalState, CancelRunParams, GetArtifactParams, GetWorkflowParams,
    LineageParams, ListApprovalsParams, ListArtifactsParams, ListRunsParams, PartitionStatusParams,
    PlanBackfillParams, PutWorkflowParams, RunRef, SetEnabledParams, StartRunParams, StreamRef,
    WorkflowDocument, name,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::error::{ServerError, ServerResult};
use crate::principal::{Access, Principal, authenticate, requested_workspace, resolve};
use crate::routes::AppState;
use crate::store::Role;

/// Shared state extractor.
pub type St = State<Arc<AppState>>;

/// Authenticates and resolves the workspace.
///
/// # Errors
///
/// Authentication and membership errors.
pub fn access(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    uri: &Uri,
) -> ServerResult<Access> {
    let principal = authenticate(state, method, headers)?;
    let requested = requested_workspace(headers, uri.query())?;
    resolve(state, principal, requested)
}

/// Parses a JSON body into a method parameter type, with an O-00 error on failure.
///
/// # Errors
///
/// `api.invalid_params`.
pub fn body<T: DeserializeOwned>(bytes: &Bytes) -> ServerResult<T> {
    serde_json::from_slice(bytes).map_err(|e| ServerError::invalid(format!("request body: {e}")))
}

/// Query parameters as a map (last value wins).
#[must_use]
pub fn query(uri: &Uri) -> HashMap<String, String> {
    uri.query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default()
}

fn parse_opt<T: std::str::FromStr>(
    q: &HashMap<String, String>,
    key: &str,
) -> ServerResult<Option<T>> {
    q.get(key)
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse()
                .map_err(|_| ServerError::invalid(format!("query parameter {key} is malformed")))
        })
        .transpose()
}

/// Relays one call after the role and method checks.
///
/// # Errors
///
/// Role, policy and daemon errors.
pub async fn relay<P: Serialize>(
    state: &AppState,
    a: &Access,
    method: &str,
    role: Role,
    params: &P,
) -> ServerResult<Value> {
    a.require(role)?;
    a.require_method(method)?;
    let pool = state.pool(&a.workspace)?;
    let params = serde_json::to_value(params).map_err(|e| ServerError::internal(e.to_string()))?;
    let mutating = ogun_api::policy::lookup(method).is_some_and(|p| p.mutating);
    let result = pool.call_value(method, params).await;
    if mutating {
        tracing::info!(
            who = %a.principal.label(),
            workspace = %a.workspace.workspace_id,
            method,
            ok = result.is_ok(),
            "relayed"
        );
    }
    result
}

fn ok(v: Value) -> Response {
    Json(v).into_response()
}

fn respond(r: ServerResult<Value>) -> Response {
    match r {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

/// Authenticates, binds `a` to the caller's [`Access`] and runs the body, answering its result or its error.
macro_rules! handler {
    ($state:ident, $method:ident, $headers:ident, $uri:ident, $a:ident, $body:block) => {{
        let r: ServerResult<Value> = async move {
            let $a = access(&$state, &$method, &$headers, &$uri)?;
            $body
        }
        .await;
        respond(r)
    }};
}

/// `GET /api/v1/status`.
pub async fn status(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    handler!(s, m, h, u, a, {
        relay(&s, &a, name::DAEMON_STATUS, Role::Viewer, &json!({})).await
    })
}

/// `GET /api/v1/me`.
pub async fn me(State(s): St, m: Method, h: HeaderMap) -> Response {
    let r: ServerResult<Value> = (|| {
        let p = authenticate(&s, &m, &h)?;
        let user = p.user().clone();
        match &p {
            Principal::Session {
                csrf, created_at, ..
            } => {
                let mut workspaces = Vec::new();
                for mem in s.store.memberships_of(user.user_id)? {
                    if let Some(w) = s.store.workspace(mem.workspace_id)? {
                        workspaces.push(json!({
                            "workspace_id": w.workspace_id,
                            "name": w.name,
                            "role": mem.role,
                            "approver": mem.approver,
                        }));
                    }
                }
                let absolute =
                    std::time::Duration::from_secs(s.cfg.sessions.absolute_timeout_hours * 3600);
                Ok(json!({
                    "kind": "session",
                    "user": user,
                    "workspaces": workspaces,
                    "csrf_token": csrf,
                    "session_expires_at": created_at.checked_add(absolute),
                    "remote_approvals": s.cfg.remote_approvals,
                    "rp_id": s.derived.rp_id,
                }))
            }
            Principal::Token { token, .. } => Ok(json!({
                "kind": "token",
                "acts_as": "agent",
                "user": user,
                "token": {
                    "token_id": token.token_id,
                    "name": token.name,
                    "workspace_id": token.workspace_id,
                    "role": token.role,
                    "expires_at": token.expires_at,
                },
            })),
        }
    })();
    respond(r)
}

/// `GET /api/v1/workflows`.
pub async fn workflows_list(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    handler!(s, m, h, u, a, {
        relay(&s, &a, name::WORKFLOWS_LIST, Role::Viewer, &json!({})).await
    })
}

/// `GET /api/v1/workflows/{name}?version=`.
pub async fn workflows_get(
    State(s): St,
    Path(wf): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
) -> Response {
    handler!(s, m, h, u, a, {
        let q = query(&u);
        let p = GetWorkflowParams {
            name: wf,
            version: parse_opt(&q, "version")?,
        };
        relay(&s, &a, name::WORKFLOWS_GET, Role::Viewer, &p).await
    })
}

/// `POST /api/v1/workflows/validate`.
pub async fn workflows_validate(
    State(s): St,
    m: Method,
    h: HeaderMap,
    u: Uri,
    b: Bytes,
) -> Response {
    handler!(s, m, h, u, a, {
        let p: WorkflowDocument = body(&b)?;
        relay(&s, &a, name::WORKFLOWS_VALIDATE, Role::Viewer, &p).await
    })
}

/// `POST /api/v1/workflows`.
pub async fn workflows_put(State(s): St, m: Method, h: HeaderMap, u: Uri, b: Bytes) -> Response {
    handler!(s, m, h, u, a, {
        let p: PutWorkflowParams = body(&b)?;
        relay(&s, &a, name::WORKFLOWS_PUT, Role::Editor, &p).await
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EnabledBody {
    enabled: bool,
}

/// `POST /api/v1/workflows/{name}/enabled`.
pub async fn workflows_set_enabled(
    State(s): St,
    Path(wf): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
    b: Bytes,
) -> Response {
    handler!(s, m, h, u, a, {
        let e: EnabledBody = body(&b)?;
        let p = SetEnabledParams {
            name: wf,
            enabled: e.enabled,
        };
        relay(&s, &a, name::WORKFLOWS_SET_ENABLED, Role::Admin, &p).await
    })
}

/// Inserts the workflow name from the path into a body object; a body that names another workflow is refused.
fn with_workflow(b: &Bytes, wf: &str) -> ServerResult<Value> {
    let mut v: Value = body(b)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| ServerError::invalid("request body must be an object"))?;
    if let Some(given) = obj.get("workflow")
        && given.as_str() != Some(wf)
    {
        return Err(ServerError::invalid(
            "body names another workflow than the path",
        ));
    }
    obj.insert("workflow".to_owned(), Value::String(wf.to_owned()));
    Ok(v)
}

/// `POST /api/v1/workflows/{name}/runs`.
pub async fn runs_start(
    State(s): St,
    Path(wf): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
    b: Bytes,
) -> Response {
    handler!(s, m, h, u, a, {
        let p: StartRunParams = serde_json::from_value(with_workflow(&b, &wf)?)
            .map_err(|e| ServerError::invalid(format!("request body: {e}")))?;
        relay(&s, &a, name::RUNS_START, Role::Editor, &p).await
    })
}

/// `GET /api/v1/workflows/{name}/partitions?version=&from=&to=`.
pub async fn partitions_status(
    State(s): St,
    Path(wf): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
) -> Response {
    handler!(s, m, h, u, a, {
        let q = query(&u);
        let p = PartitionStatusParams {
            workflow: wf,
            version: parse_opt(&q, "version")?,
            from: q.get("from").cloned(),
            to: q.get("to").cloned(),
        };
        relay(&s, &a, name::PARTITIONS_STATUS, Role::Viewer, &p).await
    })
}

/// `POST /api/v1/workflows/{name}/backfill-plan`.
pub async fn backfill_plan(
    State(s): St,
    Path(wf): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
    b: Bytes,
) -> Response {
    handler!(s, m, h, u, a, {
        let p: PlanBackfillParams = serde_json::from_value(with_workflow(&b, &wf)?)
            .map_err(|e| ServerError::invalid(format!("request body: {e}")))?;
        relay(&s, &a, name::BACKFILLS_PLAN, Role::Editor, &p).await
    })
}

/// `GET /api/v1/runs?workflow=&before=&limit=`.
pub async fn runs_list(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    handler!(s, m, h, u, a, {
        let q = query(&u);
        let p = ListRunsParams {
            workflow: q.get("workflow").cloned().filter(|w| !w.is_empty()),
            before: parse_opt(&q, "before")?,
            limit: parse_opt(&q, "limit")?,
        };
        relay(&s, &a, name::RUNS_LIST, Role::Viewer, &p).await
    })
}

fn run_id(text: &str) -> ServerResult<ogun_types::RunId> {
    text.parse()
        .map_err(|_| ServerError::invalid("run_id is not an orun_ id"))
}

/// `GET /api/v1/runs/{run_id}`.
pub async fn runs_get(
    State(s): St,
    Path(id): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
) -> Response {
    handler!(s, m, h, u, a, {
        let p = RunRef {
            run_id: run_id(&id)?,
        };
        relay(&s, &a, name::RUNS_GET, Role::Viewer, &p).await
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelBody {
    idempotency_key: String,
}

/// `POST /api/v1/runs/{run_id}/cancel`.
pub async fn runs_cancel(
    State(s): St,
    Path(id): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
    b: Bytes,
) -> Response {
    handler!(s, m, h, u, a, {
        let c: CancelBody = body(&b)?;
        let p = CancelRunParams {
            run_id: run_id(&id)?,
            idempotency_key: c.idempotency_key,
        };
        relay(&s, &a, name::RUNS_CANCEL, Role::Editor, &p).await
    })
}

/// `GET /api/v1/approvals?state=`.
pub async fn approvals_list(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    handler!(s, m, h, u, a, {
        let q = query(&u);
        let state: Option<ApprovalState> =
            match q.get("state").filter(|v| !v.is_empty()) {
                Some(v) => Some(serde_json::from_value(Value::String(v.clone())).map_err(
                    |_| ServerError::invalid("state must be pending, approved, denied or expired"),
                )?),
                None => None,
            };
        relay(
            &s,
            &a,
            name::APPROVALS_LIST,
            Role::Viewer,
            &ListApprovalsParams { state },
        )
        .await
    })
}

/// Parses an approval id.
///
/// # Errors
///
/// `api.invalid_params`.
pub fn approval_id(text: &str) -> ServerResult<ogun_types::ApprovalId> {
    text.parse()
        .map_err(|_| ServerError::invalid("approval_id is not an oap_ id"))
}

/// `GET /api/v1/approvals/{approval_id}`.
pub async fn approvals_get(
    State(s): St,
    Path(id): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
) -> Response {
    handler!(s, m, h, u, a, {
        let p = ApprovalRef {
            approval_id: approval_id(&id)?,
        };
        relay(&s, &a, name::APPROVALS_GET, Role::Viewer, &p).await
    })
}

/// `GET /api/v1/artifacts?run_id=`.
pub async fn artifacts_list(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    handler!(s, m, h, u, a, {
        let q = query(&u);
        let rid = q
            .get("run_id")
            .ok_or_else(|| ServerError::invalid("run_id is required"))?;
        let p = ListArtifactsParams {
            run_id: run_id(rid)?,
        };
        relay(&s, &a, name::ARTIFACTS_LIST, Role::Viewer, &p).await
    })
}

/// Checks a BLAKE3 digest path parameter.
///
/// # Errors
///
/// `api.invalid_params`.
pub fn digest(text: &str) -> ServerResult<String> {
    if crate::util::is_blake3_hex(text) {
        Ok(text.to_owned())
    } else {
        Err(ServerError::invalid(
            "the digest must be 64 lowercase hex digits",
        ))
    }
}

/// `GET /api/v1/artifacts/{blake3}`: metadata only; the bytes are at `.../content`.
pub async fn artifacts_get(
    State(s): St,
    Path(d): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
) -> Response {
    handler!(s, m, h, u, a, {
        let d = digest(&d)?;
        let p = GetArtifactParams {
            blake3: d.clone(),
            inline_max_bytes: 0,
        };
        let v = relay(&s, &a, name::ARTIFACTS_GET, Role::Viewer, &p).await?;
        Ok(json!({
            "artifact": v.get("artifact").cloned().unwrap_or(Value::Null),
            "content_url": format!("/api/v1/artifacts/{d}/content"),
        }))
    })
}

/// `GET /api/v1/lineage?run_id=&blake3=`.
pub async fn lineage(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    handler!(s, m, h, u, a, {
        let q = query(&u);
        let p = LineageParams {
            run_id: q
                .get("run_id")
                .filter(|v| !v.is_empty())
                .map(|v| run_id(v))
                .transpose()?,
            blake3: q
                .get("blake3")
                .filter(|v| !v.is_empty())
                .map(|v| digest(v))
                .transpose()?,
        };
        if p.run_id.is_none() == p.blake3.is_none() {
            return Err(ServerError::invalid(
                "give exactly one of run_id and blake3",
            ));
        }
        relay(&s, &a, name::LINEAGE_GET, Role::Viewer, &p).await
    })
}

/// `GET /api/v1/streams`.
pub async fn streams_list(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    handler!(s, m, h, u, a, {
        relay(&s, &a, name::STREAMS_LIST, Role::Viewer, &json!({})).await
    })
}

/// `GET /api/v1/streams/{workflow}/{node}`.
pub async fn streams_status(
    State(s): St,
    Path((workflow, node)): Path<(String, String)>,
    m: Method,
    h: HeaderMap,
    u: Uri,
) -> Response {
    handler!(s, m, h, u, a, {
        relay(
            &s,
            &a,
            name::STREAMS_STATUS,
            Role::Viewer,
            &StreamRef { workflow, node },
        )
        .await
    })
}

/// `GET /api/v1/edges`.
pub async fn edges_list(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    handler!(s, m, h, u, a, {
        relay(&s, &a, name::EDGES_LIST, Role::Viewer, &json!({})).await
    })
}

/// Unknown API routes.
pub async fn not_found() -> Response {
    ServerError::not_found("no such route").into_response()
}
