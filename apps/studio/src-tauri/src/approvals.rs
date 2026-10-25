//! Approval decisions from Studio: review, arm, confirm (Zeus ADR 0007 rules on Ogun's desktop trust surface).
//!
//! 1. `approvals_prepare` reads the approval fresh from the daemon, and records the decision the person chose
//!    together with the `request_hash` the daemon reported. The webview shows the daemon-computed facts and a
//!    confirmation button that names the decision.
//! 2. The button arms after [`ARM_MS`] (the deliberate arming delay of the brand's trust-surface rule).
//! 3. `approvals_confirm` sends `approvals.decide` with `Evidence::Studio` and the hash recorded in step 1, after
//!    checking the approval still has that hash and is still pending. A confirmation is single-use, expires after
//!    [`EXPIRE_MS`], and a confirmation that arrives before the button armed is discarded.
//!
//! Studio never decides on its own and never forwards a hash supplied by the webview. The daemon verifies that the
//! connection is `human` and that the peer executable is the pinned `ogun-studio` binary; agents can never reach
//! this path (constraint 3).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ogun_api::methods::{
    ApprovalRef, ApprovalState, ApprovalView, DecideParams, Decision, Evidence, name,
};
use ogun_types::ApprovalId;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::error::{StudioError, StudioResult};
use crate::link::{Link, Studio};

/// The confirm button arms after this long.
pub const ARM_MS: u64 = 1_500;
/// A prepared decision expires after this long.
pub const EXPIRE_MS: u64 = 120_000;
/// Decisions under review at once.
const MAX_PENDING: usize = 8;

#[derive(Debug)]
struct Pending {
    approval_id: ApprovalId,
    decision: Decision,
    request_hash: String,
    prepared_at: Instant,
}

/// Decisions under review.
#[derive(Debug, Default)]
pub struct Confirmations {
    pending: Mutex<HashMap<String, Pending>>,
}

/// What the confirmation dialog shows.
#[derive(Debug, Clone, Serialize)]
pub struct Prepared {
    /// Single-use id for `approvals_confirm`.
    pub confirmation_id: String,
    /// The decision the button will send.
    pub decision: Decision,
    /// The approval as the daemon reported it just now.
    pub approval: ApprovalView,
    /// The button arms after this many milliseconds.
    pub arm_ms: u64,
    /// The confirmation expires after this many milliseconds.
    pub expires_ms: u64,
}

fn confirmation_id() -> StudioResult<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| StudioError::unavailable("no OS random source"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

async fn fetch(studio: &Studio, approval_id: ApprovalId) -> StudioResult<ApprovalView> {
    let v = studio
        .call(
            name::APPROVALS_GET,
            serde_json::to_value(ApprovalRef { approval_id })
                .map_err(|e| StudioError::invalid(e.to_string()))?,
        )
        .await?;
    serde_json::from_value(v)
        .map_err(|_| StudioError::new("internal.bug", "unexpected approval view from the daemon"))
}

async fn require_local_human(studio: &Studio) -> StudioResult<()> {
    match studio.link().await {
        #[cfg(desktop)]
        Link::Local(local) => {
            let s = local.session().await?;
            if s.kind == ogun_api::ClientKind::Human {
                Ok(())
            } else {
                Err(StudioError::new(
                    "approval.channel_required",
                    "the studio credential is not a human credential; approvals need one",
                ))
            }
        }
        _ => Err(StudioError::new(
            "approval.channel_required",
            "approvals are decided in Ogun Studio on the desktop, in the ogun CLI on a terminal, or in Studio-web with a passkey",
        )),
    }
}

impl Confirmations {
    /// Step 1: review an approval and record the decision under a single-use confirmation id.
    ///
    /// # Errors
    ///
    /// `approval.channel_required` off the desktop, `approval.already_decided` for a decided approval, or the
    /// daemon's error.
    pub async fn prepare(
        &self,
        studio: &Studio,
        approval_id: ApprovalId,
        decision: Decision,
    ) -> StudioResult<Prepared> {
        require_local_human(studio).await?;
        let approval = fetch(studio, approval_id).await?;
        if approval.state != ApprovalState::Pending {
            return Err(StudioError::new(
                "approval.already_decided",
                "this approval is no longer pending",
            ));
        }
        let id = confirmation_id()?;
        let mut pending = self.pending.lock().await;
        let expire = Duration::from_millis(EXPIRE_MS);
        pending.retain(|_, p| p.prepared_at.elapsed() < expire);
        if pending.len() >= MAX_PENDING {
            return Err(StudioError::invalid(
                "too many decisions under review; close one first",
            ));
        }
        pending.insert(
            id.clone(),
            Pending {
                approval_id,
                decision,
                request_hash: approval.request_hash.clone(),
                prepared_at: Instant::now(),
            },
        );
        Ok(Prepared {
            confirmation_id: id,
            decision,
            approval,
            arm_ms: ARM_MS,
            expires_ms: EXPIRE_MS,
        })
    }

    /// Discards a decision under review (the person closed the dialog).
    pub async fn discard(&self, confirmation_id: &str) {
        self.pending.lock().await.remove(confirmation_id);
    }

    /// Step 3: send the decision with `studio` evidence.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` for an unknown, early or expired confirmation; `approval.hash_mismatch` when the
    /// approval changed since the review; the daemon's error otherwise (for example `approval.channel_required`
    /// when the peer is not the pinned binary).
    pub async fn confirm(
        &self,
        studio: &Studio,
        confirmation_id: &str,
    ) -> StudioResult<ApprovalView> {
        let p = self
            .pending
            .lock()
            .await
            .remove(confirmation_id)
            .ok_or_else(|| {
                StudioError::invalid("no such decision under review; review the approval again")
            })?;
        let elapsed = p.prepared_at.elapsed();
        if elapsed < Duration::from_millis(ARM_MS) {
            return Err(StudioError::invalid(
                "the decision was confirmed before the button armed; review the approval again",
            ));
        }
        if elapsed > Duration::from_millis(EXPIRE_MS) {
            return Err(StudioError::invalid(
                "the review expired; review the approval again",
            ));
        }
        require_local_human(studio).await?;
        let now = fetch(studio, p.approval_id).await?;
        if now.request_hash != p.request_hash {
            return Err(StudioError::new(
                "approval.hash_mismatch",
                "the approval changed after you reviewed it; review it again",
            ));
        }
        if now.state != ApprovalState::Pending {
            return Err(StudioError::new(
                "approval.already_decided",
                "this approval is no longer pending",
            ));
        }
        let params = DecideParams {
            approval_id: p.approval_id,
            decision: p.decision,
            request_hash: p.request_hash,
            evidence: Evidence::Studio,
        };
        let v = studio
            .call(
                name::APPROVALS_DECIDE,
                serde_json::to_value(params).map_err(|e| StudioError::invalid(e.to_string()))?,
            )
            .await?;
        serde_json::from_value(v).map_err(|_| {
            StudioError::new("internal.bug", "unexpected approval view from the daemon")
        })
    }
}
