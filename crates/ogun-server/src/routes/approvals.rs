//! Passkeys and remote approval decisions (O-13 section 8; O-15 section 7).
//!
//! Every route here needs a person's browser session with the approver flag; API tokens never reach them. A
//! passkey is verified, then proposed to the daemon with `approvers.propose`; it signs nothing until a person on
//! the host runs `ogun approvers confirm <credential_id> <fingerprint>`. A decision is relayed only when
//! `remote_approvals` is on, and only with an assertion that the bridge has verified and the daemon verifies again.

use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, Uri};
use axum::response::{IntoResponse, Response};
use ogun_api::methods::{
    ApprovalRef, ApprovalState, ApprovalView, ApproverView, DecideParams, Decision, Evidence,
    ListApproversResult, ProposeApproverParams, name,
};
use ogun_types::Timestamp;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Attestation;
use crate::error::{ServerError, ServerResult};
use crate::principal::{Access, Principal};
use crate::routes::AppState;
use crate::routes::api::{St, access, approval_id, body};
use crate::store::{Passkey, RegistrationChallenge};
use crate::util::{b64url, b64url_decode, random_bytes};
use crate::webauthn::{approval_challenge, fingerprint, verify_assertion, verify_registration};

const CEREMONY_TIMEOUT_MS: u64 = 120_000;

fn session_key(a: &Access) -> ServerResult<&str> {
    match &a.principal {
        Principal::Session { key, .. } => Ok(key),
        Principal::Token { .. } => Err(ServerError::denied("API tokens cannot use passkeys")),
    }
}

fn reply(r: ServerResult<Value>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn confirmed_keys(s: &AppState, a: &Access) -> ServerResult<Vec<ApproverView>> {
    let pool = s.pool(&a.workspace)?;
    let list: ListApproversResult = pool.call(name::APPROVERS_LIST, &json!({})).await?;
    let me = a.principal.user().user_id.to_string();
    Ok(list
        .approvers
        .into_iter()
        .filter(|v| v.user == me && v.rp_id == s.derived.rp_id)
        .collect())
}

/// `POST /api/v1/passkeys/register-options`: `PublicKeyCredentialCreationOptions` as JSON (binary members in
/// base64url).
pub async fn register_options(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    reply(async {
        let a = access(&s, &m, &h, &u)?;
        a.require_approver()?;
        let key = session_key(&a)?;
        let user = a.principal.user();
        let challenge = random_bytes::<32>()?;
        s.store.put_challenge(
            key,
            &RegistrationChallenge {
                challenge: b64url(challenge.as_slice()),
                user_id: user.user_id,
                workspace_id: a.workspace.workspace_id,
                created_at: Timestamp::now(),
            },
        )?;
        let exclude: Vec<Value> = s
            .store
            .passkeys_of(user.user_id, a.workspace.workspace_id)?
            .into_iter()
            .map(|p| json!({ "type": "public-key", "id": p.credential_id }))
            .collect();
        Ok(json!({
            "rp": { "id": s.derived.rp_id, "name": s.derived.rp_name },
            "user": {
                "id": b64url(user.user_id.to_string().as_bytes()),
                "name": user.email.clone().unwrap_or_else(|| user.user_id.to_string()),
                "displayName": user.display_name,
            },
            "challenge": b64url(challenge.as_slice()),
            "pubKeyCredParams": [{ "type": "public-key", "alg": crate::webauthn::ALG_ES256 }],
            "timeout": CEREMONY_TIMEOUT_MS,
            "attestation": match s.cfg.webauthn.attestation { Attestation::None => "none", Attestation::Direct => "direct" },
            "authenticatorSelection": { "userVerification": "required", "residentKey": "preferred" },
            "excludeCredentials": exclude,
        }))
    }
    .await)
}

#[derive(Deserialize)]
struct RegisterBody {
    credential_id: String,
    client_data_json: String,
    attestation_object: String,
}

/// `POST /api/v1/passkeys/register`: verifies the attestation, proposes the key to the daemon and returns the
/// fingerprint the host person must confirm.
pub async fn register(State(s): St, m: Method, h: HeaderMap, u: Uri, b: Bytes) -> Response {
    reply(async {
        let a = access(&s, &m, &h, &u)?;
        a.require_approver()?;
        let key = session_key(&a)?;
        let req: RegisterBody = body(&b)?;
        let user = a.principal.user().clone();
        let ch = s
            .store
            .take_challenge(key)?
            .ok_or_else(|| ServerError::webauthn("no passkey registration is in progress"))?;
        let fresh = ch
            .created_at
            .checked_add(Duration::from_millis(CEREMONY_TIMEOUT_MS * 2))
            .is_some_and(|t| t > Timestamp::now());
        if ch.user_id != user.user_id || ch.workspace_id != a.workspace.workspace_id || !fresh {
            return Err(ServerError::webauthn("the registration challenge has expired; start again"));
        }
        let challenge = b64url_decode(&ch.challenge, "challenge")?;
        let reg = verify_registration(
            &s.derived.rp_id,
            &s.derived.origin,
            &challenge,
            &req.credential_id,
            &req.client_data_json,
            &req.attestation_object,
        )?;
        if s.store.passkey(&reg.credential_id)?.is_some() {
            return Err(ServerError::webauthn("this passkey is already registered"));
        }
        let pool = s.pool(&a.workspace)?;
        let view: ApproverView = pool
            .call(
                name::APPROVERS_PROPOSE,
                &ProposeApproverParams {
                    credential_id: reg.credential_id.clone(),
                    public_key: b64url(&reg.sec1),
                    user: user.user_id.to_string(),
                    rp_id: s.derived.rp_id.clone(),
                },
            )
            .await?;
        let local = fingerprint(&reg.sec1);
        if view.fingerprint != local {
            tracing::warn!(credential = %reg.credential_id, "the daemon's fingerprint differs from the bridge's");
        }
        s.store.put_passkey(&Passkey {
            credential_id: reg.credential_id.clone(),
            user_id: user.user_id,
            workspace_id: a.workspace.workspace_id,
            public_key: b64url(&reg.sec1),
            fingerprint: view.fingerprint.clone(),
            sign_count: reg.sign_count,
            rp_id: s.derived.rp_id.clone(),
            created_at: Timestamp::now(),
        })?;
        tracing::info!(who = %user.user_id, workspace = %a.workspace.workspace_id, fmt = %reg.fmt, "passkey proposed");
        Ok(json!({
            "approver": view,
            "attestation_format": reg.fmt,
            "confirm_command": format!("ogun approvers confirm {} {}", view.credential_id, view.fingerprint),
            "note": "This passkey cannot sign decisions yet. A person at the host's terminal must run the confirm command, checking that the fingerprint matches the one shown here.",
        }))
    }
    .await)
}

/// `GET /api/v1/passkeys`: the caller's passkeys in the workspace, with their confirmation state.
pub async fn passkeys_list(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    reply(async {
        let a = access(&s, &m, &h, &u)?;
        a.require_session()?;
        let mine = s
            .store
            .passkeys_of(a.principal.user().user_id, a.workspace.workspace_id)?;
        let confirmed = if a.approver { confirmed_keys(&s, &a).await? } else { vec![] };
        let list: Vec<Value> = mine
            .into_iter()
            .map(|p| {
                let c = confirmed
                    .iter()
                    .find(|v| v.credential_id == p.credential_id)
                    .is_some_and(|v| v.confirmed);
                json!({
                    "credential_id": p.credential_id,
                    "fingerprint": p.fingerprint,
                    "created_at": p.created_at,
                    "confirmed": c,
                    "confirm_command": format!("ogun approvers confirm {} {}", p.credential_id, p.fingerprint),
                })
            })
            .collect();
        Ok(json!({ "passkeys": list }))
    }
    .await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionsBody {
    decision: Decision,
}

fn decision_word(d: Decision) -> &'static str {
    match d {
        Decision::Approve => "approve",
        Decision::Deny => "deny",
    }
}

async fn pending(
    s: &AppState,
    a: &Access,
    id: ogun_types::ApprovalId,
) -> ServerResult<ApprovalView> {
    let pool = s.pool(&a.workspace)?;
    let v: ApprovalView = pool
        .call(name::APPROVALS_GET, &ApprovalRef { approval_id: id })
        .await?;
    if v.state != ApprovalState::Pending {
        return Err(ServerError::new(
            "approval.already_decided",
            "the approval is no longer pending",
        ));
    }
    Ok(v)
}

/// `POST /api/v1/approvals/{approval_id}/webauthn-options`: `PublicKeyCredentialRequestOptions` whose challenge
/// is `base64url(BLAKE3(approval_id || request_hash || decision))`.
pub async fn decision_options(
    State(s): St,
    Path(id): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
    b: Bytes,
) -> Response {
    reply(
        async {
            let a = access(&s, &m, &h, &u)?;
            a.require_approver()?;
            let req: OptionsBody = body(&b)?;
            let id = approval_id(&id)?;
            let approval = pending(&s, &a, id).await?;
            let allow: Vec<Value> = confirmed_keys(&s, &a)
                .await?
                .into_iter()
                .filter(|v| v.confirmed)
                .map(|v| json!({ "type": "public-key", "id": v.credential_id }))
                .collect();
            if allow.is_empty() {
                return Err(ServerError::new(
                    "approval.channel_required",
                    "register a passkey and have it confirmed on the host terminal first",
                ));
            }
            let challenge = approval_challenge(
                &id.to_string(),
                &approval.request_hash,
                decision_word(req.decision),
            );
            Ok(json!({
                "challenge": b64url(&challenge),
                "rpId": s.derived.rp_id,
                "allowCredentials": allow,
                "userVerification": "required",
                "timeout": CEREMONY_TIMEOUT_MS,
            }))
        }
        .await,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Assertion {
    credential_id: String,
    client_data_json: String,
    authenticator_data: String,
    signature: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideBody {
    decision: Decision,
    request_hash: String,
    assertion: Assertion,
}

/// `POST /api/v1/approvals/{approval_id}/decide`: relays `approvals.decide` with WebAuthn evidence.
pub async fn decide(
    State(s): St,
    Path(id): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
    b: Bytes,
) -> Response {
    reply(async {
        let a = access(&s, &m, &h, &u)?;
        if !s.cfg.remote_approvals {
            return Err(ServerError::new(
                "server.remote_approvals_disabled",
                "remote approvals are off on this server (ADR 0010); decide on the host with `ogun approvals decide`",
            ));
        }
        a.require_approver()?;
        let req: DecideBody = body(&b)?;
        let id = approval_id(&id)?;
        let user = a.principal.user().clone();
        let passkey = s
            .store
            .passkey(&req.assertion.credential_id)?
            .filter(|p| p.user_id == user.user_id && p.workspace_id == a.workspace.workspace_id)
            .ok_or_else(|| ServerError::new("approval.channel_required", "this passkey is not yours in this workspace"))?;
        let confirmed = confirmed_keys(&s, &a)
            .await?
            .into_iter()
            .any(|v| v.confirmed && v.credential_id == passkey.credential_id);
        if !confirmed {
            return Err(ServerError::new(
                "approval.channel_required",
                "this passkey has not been confirmed on the host terminal",
            ));
        }
        let approval = pending(&s, &a, id).await?;
        if approval.request_hash != req.request_hash {
            return Err(ServerError::new(
                "approval.hash_mismatch",
                "the approval changed since you looked at it; review it again",
            ));
        }
        let challenge = approval_challenge(&id.to_string(), &approval.request_hash, decision_word(req.decision));
        let sec1 = b64url_decode(&passkey.public_key, "stored key")?;
        let count = verify_assertion(
            &s.derived.rp_id,
            &s.derived.origin,
            &challenge,
            &sec1,
            &req.assertion.client_data_json,
            &req.assertion.authenticator_data,
            &req.assertion.signature,
        )?;
        s.store.bump_sign_count(&passkey.credential_id, count)?;
        let pool = s.pool(&a.workspace)?;
        let params = DecideParams {
            approval_id: id,
            decision: req.decision,
            request_hash: req.request_hash,
            evidence: Evidence::Webauthn {
                credential_id: req.assertion.credential_id,
                client_data_json: req.assertion.client_data_json,
                authenticator_data: req.assertion.authenticator_data,
                signature: req.assertion.signature,
                user: user.user_id.to_string(),
            },
        };
        let result: ServerResult<Value> = pool.call(name::APPROVALS_DECIDE, &params).await;
        tracing::info!(
            who = %user.user_id,
            workspace = %a.workspace.workspace_id,
            approval = %id,
            decision = decision_word(req.decision),
            ok = result.is_ok(),
            "remote approval decision relayed"
        );
        result
    }
    .await)
}
