//! WebAuthn approver keys and assertion checks (O-15 section 7).
//!
//! A bridge proposes an ES256 key for one of its users; a person confirms it on the host terminal by
//! fingerprint; only then may the key sign decisions. The daemon checks every assertion itself:
//! `type`, the challenge bound to the approval and decision, the relying party hash, the
//! user-present and user-verified flags, and the signature.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ogun_api::methods::{ApprovalView, ApproverView, Decision, ProposeApproverParams};
use ogun_api::{ApiError, ApiResult};
use ogun_store::Collection;
use ogun_types::Timestamp;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::store_err;
use crate::state::Daemon;

/// An approver key as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproverDoc {
    /// What clients see.
    pub view: ApproverView,
    /// Uncompressed SEC1 P-256 public key, base64url.
    pub public_key: String,
}

/// An assertion relayed by a bridge.
#[derive(Debug, Clone, Copy)]
pub struct Assertion<'a> {
    /// WebAuthn credential id.
    pub credential_id: &'a str,
    /// `clientDataJSON`, base64url.
    pub client_data_json: &'a str,
    /// `authenticatorData`, base64url.
    pub authenticator_data: &'a str,
    /// DER signature, base64url.
    pub signature: &'a str,
    /// The server user.
    pub user: &'a str,
}

fn b64(text: &str) -> ApiResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| ApiError::invalid_params("not base64url"))
}

/// The challenge a decision must sign: `base64url(BLAKE3(approval_id || request_hash || decision))`.
#[must_use]
pub fn challenge(approval_id: &str, request_hash: &str, decision: Decision) -> String {
    let d = match decision {
        Decision::Approve => "approve",
        Decision::Deny => "deny",
    };
    let mut h = blake3::Hasher::new();
    h.update(approval_id.as_bytes());
    h.update(request_hash.as_bytes());
    h.update(d.as_bytes());
    URL_SAFE_NO_PAD.encode(h.finalize().as_bytes())
}

fn fingerprint(public_key: &[u8]) -> String {
    blake3::hash(public_key).to_hex().as_str()[..16].to_owned()
}

/// `approvers.propose`.
///
/// # Errors
///
/// `api.invalid_params` for a key that is not a P-256 point.
pub fn propose(d: &Daemon, p: &ProposeApproverParams) -> ApiResult<ApproverView> {
    let key = b64(&p.public_key)?;
    VerifyingKey::from_sec1_bytes(&key)
        .map_err(|_| ApiError::invalid_params("public_key is not a P-256 point"))?;
    if p.credential_id.is_empty()
        || p.credential_id.len() > 1024
        || p.user.is_empty()
        || p.rp_id.is_empty()
    {
        return Err(ApiError::invalid_params(
            "credential_id, user and rp_id are required",
        ));
    }
    if d.store
        .get_doc::<ApproverDoc>(Collection::Approvers, &p.credential_id)
        .map_err(store_err)?
        .is_some()
    {
        return Err(ApiError::new(
            "api.idempotency_conflict",
            "that approver key is already known",
        ));
    }
    let doc = ApproverDoc {
        view: ApproverView {
            credential_id: p.credential_id.clone(),
            user: p.user.clone(),
            rp_id: p.rp_id.clone(),
            fingerprint: fingerprint(&key),
            confirmed: false,
            created_at: Timestamp::now(),
        },
        public_key: p.public_key.clone(),
    };
    d.store
        .put_doc(Collection::Approvers, &p.credential_id, &doc)
        .map_err(store_err)?;
    Ok(doc.view)
}

/// `approvers.confirm`: the person typed the fingerprint they read on the server's page.
///
/// # Errors
///
/// `credential.not_found`, or `approval.hash_mismatch` when the fingerprint differs.
pub fn confirm(
    d: &Daemon,
    credential_id: &str,
    fingerprint: Option<&str>,
) -> ApiResult<ApproverView> {
    let mut doc: ApproverDoc = d
        .store
        .get_doc(Collection::Approvers, credential_id)
        .map_err(store_err)?
        .ok_or_else(|| ApiError::new("credential.not_found", "no such approver key"))?;
    if fingerprint != Some(doc.view.fingerprint.as_str()) {
        return Err(ApiError::new(
            "approval.hash_mismatch",
            "the fingerprint does not match the proposed key",
        ));
    }
    doc.view.confirmed = true;
    d.store
        .put_doc(Collection::Approvers, credential_id, &doc)
        .map_err(store_err)?;
    Ok(doc.view)
}

/// `approvers.revoke`.
///
/// # Errors
///
/// `credential.not_found`.
pub fn revoke(d: &Daemon, credential_id: &str) -> ApiResult<ApproverView> {
    let doc: ApproverDoc = d
        .store
        .get_doc(Collection::Approvers, credential_id)
        .map_err(store_err)?
        .ok_or_else(|| ApiError::new("credential.not_found", "no such approver key"))?;
    d.store
        .delete_doc(Collection::Approvers, credential_id)
        .map_err(store_err)?;
    Ok(doc.view)
}

/// `approvers.list`.
///
/// # Errors
///
/// A store error.
pub fn list(d: &Daemon) -> ApiResult<Vec<ApproverView>> {
    Ok(d.store
        .list_docs::<ApproverDoc>(Collection::Approvers, "")
        .map_err(store_err)?
        .into_iter()
        .map(|(_, a)| a.view)
        .collect())
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
}

fn origin_matches(origin: &str, rp_id: &str) -> bool {
    let Some(rest) = origin.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split([':', '/']).next().unwrap_or("");
    host == rp_id || host.ends_with(&format!(".{rp_id}"))
}

/// Verifies a decision's assertion.
///
/// # Errors
///
/// `approval.channel_required` for every failure (the reason is logged, not returned).
pub fn verify_decision(
    d: &Daemon,
    view: &ApprovalView,
    decision: Decision,
    a: &Assertion<'_>,
) -> ApiResult<()> {
    let fail = |why: &str| {
        tracing::warn!(approval = %view.approval_id, reason = why, "webauthn assertion refused");
        ApiError::new(
            "approval.channel_required",
            "the passkey assertion does not verify",
        )
    };
    let doc: ApproverDoc = d
        .store
        .get_doc(Collection::Approvers, a.credential_id)
        .map_err(store_err)?
        .ok_or_else(|| fail("unknown approver key"))?;
    if !doc.view.confirmed {
        return Err(fail("approver key not confirmed on the host"));
    }
    if doc.view.user != a.user {
        return Err(fail("key belongs to another user"));
    }
    let client_data = b64(a.client_data_json).map_err(|_| fail("clientDataJSON encoding"))?;
    let auth_data = b64(a.authenticator_data).map_err(|_| fail("authenticatorData encoding"))?;
    let sig = b64(a.signature).map_err(|_| fail("signature encoding"))?;
    let cd: ClientData =
        serde_json::from_slice(&client_data).map_err(|_| fail("clientDataJSON shape"))?;
    if cd.kind != "webauthn.get" {
        return Err(fail("type"));
    }
    if cd.challenge.trim_end_matches('=')
        != challenge(&view.approval_id.to_string(), &view.request_hash, decision)
    {
        return Err(fail("challenge"));
    }
    if !origin_matches(&cd.origin, &doc.view.rp_id) {
        return Err(fail("origin"));
    }
    if auth_data.len() < 37 {
        return Err(fail("authenticatorData length"));
    }
    if auth_data[..32] != Sha256::digest(doc.view.rp_id.as_bytes())[..] {
        return Err(fail("rpIdHash"));
    }
    let flags = auth_data[32];
    if flags & 0x01 == 0 || flags & 0x04 == 0 {
        return Err(fail("user presence and verification are required"));
    }
    let key_bytes = b64(&doc.public_key).map_err(|_| fail("stored key"))?;
    let key = VerifyingKey::from_sec1_bytes(&key_bytes).map_err(|_| fail("stored key"))?;
    let signature = Signature::from_der(&sig).map_err(|_| fail("signature DER"))?;
    let mut msg = auth_data.clone();
    msg.extend_from_slice(&Sha256::digest(&client_data));
    key.verify(&msg, &signature).map_err(|_| fail("signature"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_challenges() {
        assert!(origin_matches(
            "https://ogun.example.org",
            "ogun.example.org"
        ));
        assert!(origin_matches("https://a.example.org:8443", "example.org"));
        assert!(!origin_matches(
            "http://ogun.example.org",
            "ogun.example.org"
        ));
        assert!(!origin_matches("https://evil-example.org", "example.org"));
        let a = challenge("oap_x", "h", Decision::Approve);
        assert_ne!(a, challenge("oap_x", "h", Decision::Deny));
    }
}
