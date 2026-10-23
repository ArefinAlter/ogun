//! Hub side of edge-to-hub sync (O-11): `sync.offer` and `sync.chunk`.
//!
//! The daemon verifies everything itself; the relay in front of it (`ogun-server`) only rate-limits.
//! An offer must be signed by the key a person enrolled for that edge (`edges.enroll`) and may only
//! name assets in the edge's own namespace. A chunk is accepted only for a digest a verified offer
//! announced, and a completed object is checked against its BLAKE3 before it is imported into the
//! artifact store, with the edge, asset, partition and revision as metadata.

use std::collections::BTreeSet;

use ogun_api::methods::{
    EdgeView, SyncChunkParams, SyncChunkResult, SyncOfferParams, SyncOfferResult, name,
};
use ogun_api::{ApiError, ApiResult};
use ogun_store::Collection;
use ogun_sync::hub::{ChunkOutcome, HubError, answer_offer, verify_offer_params};
use ogun_types::Timestamp;
use serde_json::Value;

use crate::error::{artifact_err, store_err};
use crate::server::{params, result};
use crate::state::Daemon;

fn hub_err(e: &HubError) -> ApiError {
    ApiError::new(e.code(), e.to_string())
}

fn edge(d: &Daemon, id: &str) -> ApiResult<Option<EdgeView>> {
    d.store.get_doc(Collection::Edges, id).map_err(store_err)
}

fn held(d: &Daemon, digest: &str) -> bool {
    zeus_types::Hex256::from_hex(digest).is_ok_and(|h| {
        d.artifacts
            .list(&zeus_artifact::ArtifactFilter {
                blake3: Some(h),
                limit: 1,
                ..Default::default()
            })
            .is_ok_and(|v| !v.is_empty())
    })
}

/// Handles one sync method.
///
/// # Errors
///
/// `edge.not_enrolled`, `sync.bad_signature`, `sync.digest_mismatch`, `api.permission_denied`
/// (outside the edge's namespace) or `api.invalid_params`.
pub fn handle(d: &Daemon, method: &str, p: Value) -> ApiResult<Value> {
    match method {
        name::SYNC_OFFER => {
            let p: SyncOfferParams = params(p)?;
            let e = edge(d, &p.edge_id.to_string())?;
            let offer = verify_offer_params(&p, e.as_ref().map(|e| e.public_key.as_str()))
                .map_err(|e| hub_err(&e))?;
            let have: BTreeSet<String> = offer
                .items
                .iter()
                .filter(|i| held(d, &i.blake3))
                .map(|i| i.blake3.clone())
                .collect();
            let want = answer_offer(&d.hub, &offer, &have).map_err(|e| hub_err(&e))?;
            result(&SyncOfferResult {
                want: serde_json::to_value(&want).map_err(|e| ApiError::internal(e.to_string()))?,
            })
        }
        name::SYNC_CHUNK => {
            let p: SyncChunkParams = params(p)?;
            let edge_key = p.edge_id.to_string();
            let Some(mut e) = edge(d, &edge_key)? else {
                return Err(hub_err(&HubError::NotEnrolled));
            };
            match d.hub.put(&p).map_err(|e| hub_err(&e))? {
                ChunkOutcome::Partial { .. } => result(&SyncChunkResult { complete: false }),
                ChunkOutcome::Complete { path, item } => {
                    let mut draft = zeus_artifact::ArtifactDraft::local(
                        "ogun",
                        env!("CARGO_PKG_VERSION"),
                        Some(format!("{}.jsonl", item.partition)),
                    );
                    draft.record.declared_media_type = Some("application/x-ndjson".into());
                    for (k, v) in [
                        ("ogun.edge", edge_key.clone()),
                        ("ogun.asset", item.asset.clone()),
                        ("ogun.partition", item.partition.clone()),
                        ("ogun.output", "window".to_owned()),
                    ] {
                        draft.record.metadata.insert(k.into(), Value::String(v));
                    }
                    draft
                        .record
                        .metadata
                        .insert("ogun.revision".into(), Value::from(item.revision));
                    d.artifacts
                        .ingest(&path, draft, Some(&format!("ogun-sync-{}", item.blake3)))
                        .map_err(|e| artifact_err(&e))?;
                    d.hub
                        .discard_complete(&edge_key, &item.blake3)
                        .map_err(|e| hub_err(&e))?;
                    e.last_sync_at = Some(Timestamp::now());
                    d.store
                        .put_doc(Collection::Edges, &edge_key, &e)
                        .map_err(store_err)?;
                    tracing::info!(edge = %edge_key, partition = %item.partition, revision = item.revision, "edge object imported");
                    result(&SyncChunkResult { complete: true })
                }
            }
        }
        _ => Err(ApiError::new("api.method_not_found", "no such method")),
    }
}
