//! Workspace administration (O-13 sections 3 and 5): members, invitations and API tokens. These routes are the
//! server's own (no daemon method) and need a person's browser session; API tokens cannot manage anything.
//!
//! Rules: admins manage viewers and editors; only owners grant or change admin and owner roles and the approver
//! flag; a workspace always keeps one owner. A token's role is viewer or editor and never above its creator's.

use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, Uri};
use axum::response::{IntoResponse, Response};
use ogun_types::{Timestamp, TokenId, UserId, WorkspaceId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{ServerError, ServerResult};
use crate::principal::{Access, authenticate, mint_token, resolve};
use crate::routes::AppState;
use crate::routes::api::{St, access, body};
use crate::store::{Invite, Role, TokenRecord};
use crate::util::{random_token, secret_key};

/// Context for invitation codes.
pub const INVITE_CONTEXT: &str = "ogun-server 2026-10 invite v1";

fn reply(r: ServerResult<Value>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

fn workspace_access(s: &AppState, m: &Method, h: &HeaderMap, wsp: &str) -> ServerResult<Access> {
    let w: WorkspaceId = wsp
        .parse()
        .map_err(|_| ServerError::invalid("workspace_id is not a wsp_ id"))?;
    let p = authenticate(s, m, h)?;
    let a = resolve(s, p, Some(w))?;
    a.require_session()?;
    Ok(a)
}

/// Creates an invitation and returns its link. Used by the REST route and by `ogun-server init` and `invite`.
///
/// # Errors
///
/// Storage errors.
pub fn create_invite(
    s_store: &crate::store::Store,
    public_origin: &str,
    ttl_hours: u64,
    workspace: WorkspaceId,
    role: Role,
    approver: bool,
    created_by: Option<UserId>,
) -> ServerResult<(String, Timestamp)> {
    let code = random_token()?;
    let now = Timestamp::now();
    let expires_at = now
        .checked_add(Duration::from_secs(ttl_hours.max(1) * 3600))
        .ok_or_else(|| ServerError::internal("bad invitation lifetime"))?;
    s_store.put_invite(
        &secret_key(INVITE_CONTEXT, code.as_bytes()),
        &Invite {
            workspace_id: workspace,
            role,
            approver,
            created_at: now,
            expires_at,
            created_by,
        },
    )?;
    Ok((
        format!("{public_origin}/enroll/{}", code.as_str()),
        expires_at,
    ))
}

/// `GET /api/v1/workspaces`: the caller's workspaces.
pub async fn workspaces(State(s): St, m: Method, h: HeaderMap) -> Response {
    reply((|| {
        let p = authenticate(&s, &m, &h)?;
        let mut out = Vec::new();
        for mem in s.store.memberships_of(p.user().user_id)? {
            if let Some(w) = s.store.workspace(mem.workspace_id)? {
                out.push(json!({ "workspace": w, "role": mem.role, "approver": mem.approver }));
            }
        }
        Ok(json!({ "workspaces": out }))
    })())
}

/// `GET /api/v1/workspaces/{workspace_id}/members` (admin).
pub async fn members(State(s): St, Path(wsp): Path<String>, m: Method, h: HeaderMap) -> Response {
    reply((|| {
        let a = workspace_access(&s, &m, &h, &wsp)?;
        a.require(Role::Admin)?;
        let list: Vec<Value> = s
            .store
            .members_of(a.workspace.workspace_id)?
            .into_iter()
            .map(|(mem, u)| {
                json!({
                    "user_id": mem.user_id,
                    "display_name": u.as_ref().map(|u| u.display_name.clone()),
                    "email": u.as_ref().and_then(|u| u.email.clone()),
                    "role": mem.role,
                    "approver": mem.approver,
                    "added_at": mem.added_at,
                })
            })
            .collect();
        Ok(json!({ "members": list }))
    })())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberPatch {
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    approver: Option<bool>,
}

fn may_manage(actor: Role, current: Role, new: Option<Role>) -> bool {
    if actor == Role::Owner {
        return true;
    }
    actor == Role::Admin && current <= Role::Editor && new.is_none_or(|r| r <= Role::Editor)
}

/// `PATCH /api/v1/workspaces/{workspace_id}/members/{user_id}`.
pub async fn member_update(
    State(s): St,
    Path((wsp, usr)): Path<(String, String)>,
    m: Method,
    h: HeaderMap,
    b: Bytes,
) -> Response {
    reply((|| {
        let a = workspace_access(&s, &m, &h, &wsp)?;
        a.require(Role::Admin)?;
        let patch: MemberPatch = body(&b)?;
        let target: UserId = usr
            .parse()
            .map_err(|_| ServerError::invalid("user_id is not a usr_ id"))?;
        let current = s
            .store
            .membership(a.workspace.workspace_id, target)?
            .ok_or_else(|| ServerError::not_found("no such member"))?;
        if !may_manage(a.role, current.role, patch.role) {
            return Err(ServerError::denied(
                "only an owner manages admins and owners",
            ));
        }
        if patch.approver.is_some() && a.role != Role::Owner {
            return Err(ServerError::denied(
                "only an owner grants or removes the approver flag",
            ));
        }
        let updated = s.store.update_membership(
            a.workspace.workspace_id,
            target,
            patch.role,
            patch.approver,
        )?;
        tracing::info!(who = %a.principal.label(), workspace = %a.workspace.workspace_id, member = %target, role = updated.role.as_str(), approver = updated.approver, "membership changed");
        Ok(json!({ "member": updated }))
    })())
}

/// `DELETE /api/v1/workspaces/{workspace_id}/members/{user_id}`.
pub async fn member_remove(
    State(s): St,
    Path((wsp, usr)): Path<(String, String)>,
    m: Method,
    h: HeaderMap,
) -> Response {
    reply((|| {
        let a = workspace_access(&s, &m, &h, &wsp)?;
        let target: UserId = usr
            .parse()
            .map_err(|_| ServerError::invalid("user_id is not a usr_ id"))?;
        let current = s
            .store
            .membership(a.workspace.workspace_id, target)?
            .ok_or_else(|| ServerError::not_found("no such member"))?;
        let own = target == a.principal.user().user_id;
        if !own {
            a.require(Role::Admin)?;
            if !may_manage(a.role, current.role, None) {
                return Err(ServerError::denied(
                    "only an owner removes admins and owners",
                ));
            }
        }
        s.store
            .remove_membership(a.workspace.workspace_id, target)?;
        tracing::info!(who = %a.principal.label(), workspace = %a.workspace.workspace_id, member = %target, "member removed");
        Ok(json!({ "removed": target }))
    })())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteBody {
    role: Role,
    #[serde(default)]
    approver: bool,
}

/// `POST /api/v1/workspaces/{workspace_id}/invites`: a one-time enrolment link.
pub async fn invite_create(
    State(s): St,
    Path(wsp): Path<String>,
    m: Method,
    h: HeaderMap,
    b: Bytes,
) -> Response {
    reply((|| {
        let a = workspace_access(&s, &m, &h, &wsp)?;
        a.require(Role::Admin)?;
        let req: InviteBody = body(&b)?;
        if !may_manage(a.role, Role::Viewer, Some(req.role)) {
            return Err(ServerError::denied(
                "only an owner invites admins and owners",
            ));
        }
        if req.approver && a.role != Role::Owner {
            return Err(ServerError::denied(
                "only an owner grants the approver flag",
            ));
        }
        let (url, expires_at) = create_invite(
            &s.store,
            &s.derived.origin,
            s.cfg.invites.ttl_hours,
            a.workspace.workspace_id,
            req.role,
            req.approver,
            Some(a.principal.user().user_id),
        )?;
        tracing::info!(who = %a.principal.label(), workspace = %a.workspace.workspace_id, role = req.role.as_str(), "invitation created");
        Ok(json!({
            "invite_url": url,
            "expires_at": expires_at,
            "role": req.role,
            "approver": req.approver,
            "note": "The link works once. Send it to the person over a channel you trust.",
        }))
    })())
}

fn token_view(t: &TokenRecord) -> Value {
    json!({
        "token_id": t.token_id,
        "name": t.name,
        "workspace_id": t.workspace_id,
        "created_by": t.created_by,
        "role": t.role,
        "created_at": t.created_at,
        "expires_at": t.expires_at,
        "last_used_at": t.last_used_at,
        "revoked": t.revoked,
        "acts_as": "agent",
    })
}

/// `GET /api/v1/tokens`: the caller's tokens in the workspace (every token for admins).
pub async fn tokens_list(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    reply((|| {
        let a = access(&s, &m, &h, &u)?;
        a.require_session()?;
        let me = a.principal.user().user_id;
        let list: Vec<Value> = s
            .store
            .tokens()?
            .iter()
            .filter(|t| t.workspace_id == a.workspace.workspace_id)
            .filter(|t| a.role >= Role::Admin || t.created_by == me)
            .map(token_view)
            .collect();
        Ok(json!({ "tokens": list }))
    })())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenBody {
    name: String,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    expires_in_days: Option<u64>,
}

/// `POST /api/v1/tokens`: mints an API token. The secret is in this response only.
pub async fn token_create(State(s): St, m: Method, h: HeaderMap, u: Uri, b: Bytes) -> Response {
    reply((|| {
        let a = access(&s, &m, &h, &u)?;
        a.require_session()?;
        let req: TokenBody = body(&b)?;
        if req.name.is_empty() || req.name.len() > 64 || req.name.chars().any(char::is_control) {
            return Err(ServerError::invalid(
                "name must be 1 to 64 printable characters",
            ));
        }
        let role = req.role.unwrap_or(Role::Viewer);
        if role > Role::Editor {
            return Err(ServerError::invalid(
                "a token's role is viewer or editor: tokens act as agents",
            ));
        }
        a.require(role)?;
        let days = req.expires_in_days.unwrap_or(90);
        if !(1..=365).contains(&days) {
            return Err(ServerError::invalid("expires_in_days must be 1 to 365"));
        }
        let now = Timestamp::now();
        let id: TokenId = s.store.new_token_id();
        let (secret, verifier) = mint_token(id)?;
        let record = TokenRecord {
            token_id: id,
            name: req.name,
            workspace_id: a.workspace.workspace_id,
            created_by: a.principal.user().user_id,
            role,
            verifier,
            created_at: now,
            expires_at: now
                .checked_add(Duration::from_secs(days * 86_400))
                .ok_or_else(|| ServerError::internal("bad token lifetime"))?,
            last_used_at: None,
            revoked: false,
        };
        s.store.put_token(&record)?;
        tracing::info!(who = %a.principal.label(), workspace = %a.workspace.workspace_id, token = %id, role = role.as_str(), "API token created");
        Ok(json!({
            "token": token_view(&record),
            "secret": secret.as_str(),
            "note": "Shown once. The token acts as an agent: it can never decide an approval.",
        }))
    })())
}

/// `DELETE /api/v1/tokens/{token_id}`.
pub async fn token_revoke(
    State(s): St,
    Path(tok): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
) -> Response {
    reply((|| {
        let a = access(&s, &m, &h, &u)?;
        a.require_session()?;
        let id: TokenId = tok
            .parse()
            .map_err(|_| ServerError::invalid("token_id is not a tok_ id"))?;
        let mut t = s
            .store
            .token(id)?
            .filter(|t| t.workspace_id == a.workspace.workspace_id)
            .ok_or_else(|| ServerError::not_found("no such token"))?;
        if t.created_by != a.principal.user().user_id {
            a.require(Role::Admin)?;
        }
        t.revoked = true;
        s.store.put_token(&t)?;
        tracing::info!(who = %a.principal.label(), token = %id, "API token revoked");
        Ok(json!({ "token": token_view(&t) }))
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admins_manage_only_below_admin() {
        assert!(may_manage(Role::Admin, Role::Viewer, Some(Role::Editor)));
        assert!(!may_manage(Role::Admin, Role::Viewer, Some(Role::Admin)));
        assert!(!may_manage(Role::Admin, Role::Admin, Some(Role::Viewer)));
        assert!(may_manage(Role::Owner, Role::Owner, Some(Role::Admin)));
        assert!(!may_manage(Role::Editor, Role::Viewer, None));
    }
}
