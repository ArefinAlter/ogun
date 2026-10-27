//! Ogun approvals (O-07): Zeus ADR 0007 applied to Ogun's domain.
//!
//! An approval shows a person daemon-computed facts only. Its `request_hash` is BLAKE3 over the JCS
//! form of everything the decision binds to, and a decision must name it. Only two channels decide:
//! a human connection whose peer the daemon verified (the pinned `ogun` CLI on a terminal, or the
//! pinned Studio binary), and a bridge relaying a WebAuthn assertion that the daemon verifies itself
//! against an approver key a person confirmed on the host terminal. Agents and services never reach
//! a decision (the policy table refuses them before this module runs; [`decide`] checks again).

use std::time::{Duration, Instant};

use ogun_api::auth::ClientKind;
use ogun_api::methods::{
    ApprovalKind, ApprovalState, ApprovalView, Channel, DecideParams, Decision, Evidence, Fact,
};
use ogun_api::{ApiError, ApiResult};
use ogun_run::{Actor, Cause};
use ogun_store::{Collection, DocWrite};
use ogun_types::{NodeInstanceId, RunId, Timestamp};
use serde::Serialize;

use crate::error::store_err;
use crate::server::Conn;
use crate::state::{ApprovalDoc, Daemon, lock};

#[derive(Serialize)]
struct Bound<'a> {
    approval_id: String,
    kind: ApprovalKind,
    workflow: &'a str,
    workflow_hash: &'a str,
    run_id: Option<RunId>,
    node: Option<&'a str>,
    facts: &'a [Fact],
    expires_at: String,
}

fn request_hash(view: &ApprovalView) -> ApiResult<String> {
    let bound = Bound {
        approval_id: view.approval_id.to_string(),
        kind: view.kind,
        workflow: &view.workflow,
        workflow_hash: &view.workflow_hash,
        run_id: view.run_id,
        node: view.node.as_deref(),
        facts: &view.facts,
        expires_at: view.expires_at.to_rfc3339(),
    };
    let bytes = zeus_verify::canonical::to_jcs_bytes(&bound)
        .map_err(|_| ApiError::internal("facts are not hashable"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// What a new approval is about.
#[derive(Debug, Clone)]
pub struct Request {
    /// Kind.
    pub kind: ApprovalKind,
    /// Workflow name.
    pub workflow: String,
    /// The hash the decision binds to.
    pub workflow_hash: String,
    /// Run, for run-time approvals.
    pub run_id: Option<RunId>,
    /// Node, for node approvals.
    pub node: Option<String>,
    /// The waiting node instance.
    pub instance: Option<NodeInstanceId>,
    /// Facts.
    pub facts: Vec<Fact>,
    /// Code hashes covered.
    pub code: Vec<String>,
    /// Requester label.
    pub requested_by: String,
}

/// Builds a pending approval and the write that stores it; the caller commits the write together
/// with whatever the approval gates.
///
/// # Errors
///
/// `api.unavailable` when too many approvals are pending (Zeus ADR 0007: at most 50).
pub fn new_approval(d: &Daemon, req: Request) -> ApiResult<(ApprovalDoc, DocWrite)> {
    let pending = d
        .store
        .list_docs::<ApprovalDoc>(Collection::Approvals, "")
        .map_err(store_err)?
        .into_iter()
        .filter(|(_, a)| a.view.state == ApprovalState::Pending)
        .count();
    if pending >= d.config.max_pending_approvals as usize {
        return Err(ApiError::unavailable(
            "too many approvals are pending; decide or let some expire",
        ));
    }
    let now = Timestamp::now();
    let mut view = ApprovalView {
        approval_id: d.ids.approval_id(),
        kind: req.kind,
        state: ApprovalState::Pending,
        workflow: req.workflow,
        workflow_hash: req.workflow_hash,
        run_id: req.run_id,
        node: req.node,
        facts: req.facts,
        request_hash: String::new(),
        requested_by: req.requested_by,
        created_at: now,
        expires_at: now.saturating_add_ms(d.studio.runtime.approval_ttl_ms()),
        decided_at: None,
        decided_via: None,
        decided_by: None,
    };
    view.request_hash = request_hash(&view)?;
    let doc = ApprovalDoc {
        view,
        instance: req.instance,
        code: req.code,
    };
    let write = DocWrite::put(
        Collection::Approvals,
        doc.view.approval_id.to_string(),
        &doc,
    )
    .map_err(store_err)?;
    Ok((doc, write))
}

fn rate_ok(d: &Daemon) -> bool {
    let mut q = lock(&d.decisions);
    let now = Instant::now();
    while q
        .front()
        .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
    {
        q.pop_front();
    }
    if q.len() >= d.config.decisions_per_minute as usize {
        return false;
    }
    q.push_back(now);
    true
}

/// Checks the evidence against the connection. Returns the channel and who decided.
fn channel(
    d: &Daemon,
    conn: &Conn,
    doc: &ApprovalDoc,
    params: &DecideParams,
) -> ApiResult<(Channel, String)> {
    match (&params.evidence, conn.kind) {
        (Evidence::Terminal, ClientKind::Human)
            if conn.peer.has_tty && d.trusted.is_cli(conn.peer.exe.as_deref()) =>
        {
            Ok((Channel::Terminal, format!("{} (terminal)", conn.name)))
        }
        (Evidence::Studio, ClientKind::Human) if d.trusted.is_studio(conn.peer.exe.as_deref()) => {
            Ok((Channel::Studio, format!("{} (studio)", conn.name)))
        }
        (
            Evidence::Webauthn {
                credential_id,
                client_data_json,
                authenticator_data,
                signature,
                user,
            },
            ClientKind::Bridge,
        ) => {
            if !d.config.remote_approvals {
                return Err(ApiError::new(
                    "approval.channel_required",
                    "remote approvals are off (ADR 0010: they need a security review first)",
                ));
            }
            crate::webauthn::verify_decision(
                d,
                &doc.view,
                params.decision,
                &crate::webauthn::Assertion {
                    credential_id,
                    client_data_json,
                    authenticator_data,
                    signature,
                    user,
                },
            )?;
            Ok((Channel::Webauthn, format!("server user {user}")))
        }
        (_, ClientKind::Agent | ClientKind::Service) => Err(ApiError::denied(
            "agents and services never decide approvals",
        )),
        _ => Err(ApiError::new(
            "approval.channel_required",
            "this decision needs evidence the daemon can check: the ogun CLI on a terminal, Studio, or a confirmed passkey",
        )),
    }
}

/// `approvals.decide`.
///
/// # Errors
///
/// The `approval.*` codes, `api.permission_denied`, or `api.unavailable` past the rate limit.
pub fn decide(d: &Daemon, conn: &Conn, params: &DecideParams) -> ApiResult<ApprovalView> {
    let key = params.approval_id.to_string();
    let mut doc: ApprovalDoc = d
        .store
        .get_doc(Collection::Approvals, &key)
        .map_err(store_err)?
        .ok_or_else(|| ApiError::new("approval.not_found", "no such approval"))?;
    let now = Timestamp::now();
    if doc.view.state != ApprovalState::Pending {
        return Err(ApiError::new(
            "approval.already_decided",
            "this approval is no longer pending",
        ));
    }
    if now >= doc.view.expires_at {
        expire_one(d, doc)?;
        return Err(ApiError::new(
            "approval.expired",
            "this approval expired; it counts as a denial",
        ));
    }
    if params.request_hash != doc.view.request_hash {
        return Err(ApiError::new(
            "approval.hash_mismatch",
            "the decision names another request",
        ));
    }
    let (via, by) = channel(d, conn, &doc, params)?;
    if !rate_ok(d) {
        return Err(ApiError::unavailable(
            "too many decisions in the last minute",
        ));
    }
    doc.view.state = match params.decision {
        Decision::Approve => ApprovalState::Approved,
        Decision::Deny => ApprovalState::Denied,
    };
    doc.view.decided_at = Some(now);
    doc.view.decided_via = Some(via);
    doc.view.decided_by = Some(by);
    let mut writes = vec![DocWrite::put(Collection::Approvals, key, &doc).map_err(store_err)?];
    if doc.view.state == ApprovalState::Approved {
        for code in &doc.code {
            writes.push(
                DocWrite::put(
                    Collection::CodeApprovals,
                    format!("{}/{code}", doc.view.workflow),
                    &doc.view.approval_id,
                )
                .map_err(store_err)?,
            );
        }
    }
    if let (Some(instance), ApprovalState::Approved) = (doc.instance, doc.view.state) {
        writes.push(
            DocWrite::put(
                Collection::Granted,
                instance.to_string(),
                &doc.view.approval_id,
            )
            .map_err(store_err)?,
        );
    }
    match doc.instance {
        Some(instance) => {
            let cause = match params.decision {
                Decision::Approve => Cause::ApprovalGranted,
                Decision::Deny => Cause::ApprovalDenied,
            };
            d.store
                .transition_with(instance, cause, Actor::Human, now, None, &writes)
                .map_err(store_err)?;
        }
        None => d.store.write_docs(&writes).map_err(store_err)?,
    }
    tracing::info!(approval = %doc.view.approval_id, state = ?doc.view.state, channel = ?via, "approval decided");
    d.changed();
    Ok(doc.view)
}

fn expire_one(d: &Daemon, mut doc: ApprovalDoc) -> ApiResult<()> {
    let now = Timestamp::now();
    doc.view.state = ApprovalState::Expired;
    doc.view.decided_at = Some(now);
    let write = DocWrite::put(
        Collection::Approvals,
        doc.view.approval_id.to_string(),
        &doc,
    )
    .map_err(store_err)?;
    match doc.instance {
        Some(i) => {
            if let Err(e) = d.store.transition_with(
                i,
                Cause::ApprovalExpired,
                Actor::Runtime,
                now,
                None,
                std::slice::from_ref(&write),
            ) {
                // The instance may have been cancelled meanwhile; the approval still expires.
                tracing::debug!(error = %e, "expired approval's instance moved on");
                d.store.write_docs(&[write]).map_err(store_err)?;
            }
        }
        None => d.store.write_docs(&[write]).map_err(store_err)?,
    }
    Ok(())
}

/// Expires every pending approval past its deadline. Called by the executor loop.
pub fn sweep(d: &Daemon) {
    let now = Timestamp::now();
    let Ok(all) = d.store.list_docs::<ApprovalDoc>(Collection::Approvals, "") else {
        return;
    };
    let mut any = false;
    for (_, doc) in all {
        if doc.view.state == ApprovalState::Pending && now >= doc.view.expires_at {
            any |= expire_one(d, doc).is_ok();
        }
    }
    if any {
        d.changed();
    }
}

/// Every approval, newest first, optionally of one state.
///
/// # Errors
///
/// A store error.
pub fn list(d: &Daemon, state: Option<ApprovalState>) -> ApiResult<Vec<ApprovalView>> {
    let mut out: Vec<ApprovalView> = d
        .store
        .list_docs::<ApprovalDoc>(Collection::Approvals, "")
        .map_err(store_err)?
        .into_iter()
        .map(|(_, a)| a.view)
        .filter(|v| state.is_none_or(|s| v.state == s))
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(v.approval_id));
    Ok(out)
}
