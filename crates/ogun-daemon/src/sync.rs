//! Hub side of edge-to-hub sync (O-11): `sync.offer` and `sync.chunk`.
//!
//! The daemon verifies everything itself; the relay in front of it (`ogun-server`) only rate-limits.
//! An offer must be signed by the key a person enrolled for that edge (`edges.enroll`) and may only
//! name assets in the edge's own namespace. A chunk is accepted only for a digest a verified offer
//! announced, and a completed object is checked against its BLAKE3 before it is imported into the
//! artifact store, with the edge, asset, partition and revision as metadata.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use ogun_api::methods::{
    EdgeView, SyncChunkParams, SyncChunkResult, SyncOfferParams, SyncOfferResult, name,
};
use ogun_api::{ApiError, ApiResult};
use ogun_edge::Item;
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

// A catalog record alone does not prove the bytes are still available. Check the CAS object
// itself before accepting another edge's claim about it.
fn held(d: &Daemon, digest: &str) -> ApiResult<Option<zeus_artifact::Artifact>> {
    let hash = zeus_types::Hex256::from_hex(digest)
        .map_err(|_| hub_err(&HubError::BadOffer("invalid digest")))?;
    let records = d
        .artifacts
        .list(&zeus_artifact::ArtifactFilter {
            blake3: Some(hash),
            limit: 1,
            ..Default::default()
        })
        .map_err(|e| artifact_err(&e))?;
    let Some(record) = records.into_iter().next() else {
        return Ok(None);
    };
    if record.object.state != zeus_artifact::ObjectState::Present {
        return Ok(None);
    }
    let mut file = match std::fs::File::open(d.artifacts.object_path(hash)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ApiError::unavailable("cannot read sync object")),
    };
    let mut hasher = zeus_verify::hash::DualHasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| ApiError::unavailable("cannot read sync object"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    if hasher.finalize() != record.digests() {
        return Ok(None);
    }
    Ok(Some(record))
}

fn draft(edge: &str, item: &Item) -> zeus_artifact::ArtifactDraft {
    let mut draft = zeus_artifact::ArtifactDraft::local(
        "ogun",
        env!("CARGO_PKG_VERSION"),
        Some(format!("{}.jsonl", item.partition)),
    );
    draft.record.declared_media_type = Some("application/x-ndjson".into());
    // The edge may release its only copy after acknowledgement.
    draft.record.retention = zeus_artifact::Retention::Keep;
    for (k, v) in [
        ("ogun.edge", edge.to_owned()),
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
    draft
}

fn import_key(edge: &str, item: &Item) -> ApiResult<String> {
    // Hash an unambiguous tuple: bytes can be shared while every logical identity is retained.
    let identity = serde_json::to_vec(&(
        edge,
        &item.asset,
        &item.partition,
        item.revision,
        &item.blake3,
    ))
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(format!(
        "ogun-sync-item-{}",
        blake3::hash(&identity).to_hex()
    ))
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
            let mut objects = BTreeMap::new();
            // Validate all sizes before committing records or acknowledging any digest. Ack is
            // digest-based, so every offered identity for that digest must be committed first.
            for item in &offer.items {
                if !objects.contains_key(&item.blake3) {
                    objects.insert(item.blake3.clone(), held(d, &item.blake3)?);
                }
                if objects[&item.blake3]
                    .as_ref()
                    .is_some_and(|a| a.size_bytes != item.size_bytes)
                {
                    return Err(hub_err(&HubError::SizeMismatch));
                }
            }
            let mut have = BTreeSet::new();
            for item in &offer.items {
                if let Some(object) = &objects[&item.blake3] {
                    d.artifacts
                        .ingest_record_only(
                            object.digests(),
                            draft(&offer.edge_id, item),
                            Some(&import_key(&offer.edge_id, item)?),
                            object.verification.checked_at,
                        )
                        .map_err(|e| artifact_err(&e))?;
                    have.insert(item.blake3.clone());
                }
            }
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
                    d.artifacts
                        .ingest(
                            &path,
                            draft(&edge_key, &item),
                            Some(&import_key(&edge_key, &item)?),
                        )
                        .map_err(|e| artifact_err(&e))?;
                    // An ingest retry may return an old keyed record without checking CAS.
                    // Keep the verified assembly available if those bytes have since disappeared.
                    if held(d, &item.blake3)?.is_none() {
                        return Err(ApiError::unavailable(
                            "synced object is not available in the artifact store",
                        ));
                    }
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
