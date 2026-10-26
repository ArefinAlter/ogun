//! `environments.list`, `sandbox.status` and the evidence settings.

use std::sync::Arc;

use ogun_api::methods::ListEdgesResult;
use ogun_api::{ApiError, ApiResult};
use ogun_store::Collection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{args, out};
use crate::error::store_err;
use crate::server::Conn;
use crate::state::Daemon;

/// What a run's evidence bundle contains (Studio assembles the bundle from what the daemon reports).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Evidence {
    /// Offer the download on run pages.
    pub enabled: bool,
    /// `json` or `html`.
    pub format: String,
    /// Include the OpenLineage events.
    pub include_lineage: bool,
    /// Include the first 4 KiB of small text artifacts.
    pub include_previews: bool,
}

impl Default for Evidence {
    fn default() -> Self {
        Self {
            enabled: true,
            format: "json".into(),
            include_lineage: true,
            include_previews: false,
        }
    }
}

#[derive(Deserialize)]
struct EvidenceSet {
    patch: serde_json::Map<String, Value>,
}

/// The bridge authenticates at start and again whenever it reconnects; it counts as running for five minutes after.
fn ingest_recent(d: &Daemon) -> bool {
    let seen = d.ingest_seen_ms.load(std::sync::atomic::Ordering::Relaxed);
    seen > 0 && ogun_types::Timestamp::now().unix_millis() - seen < 300_000
}

fn environments(d: &Arc<Daemon>) -> ApiResult<Value> {
    let edges = ListEdgesResult {
        edges: d
            .store
            .list_docs::<ogun_api::methods::EdgeView>(Collection::Edges, "")
            .map_err(store_err)?
            .into_iter()
            .map(|(_, e)| e)
            .collect(),
    };
    let (store, note) = d.secrets.kind();
    let secrets_detail = match (store, note) {
        (crate::secrets::StoreKind::Os, _) => {
            "secret values are in the operating system's credential store"
        }
        (crate::secrets::StoreKind::EncryptedFile, _) => {
            "secret values are in a passphrase-encrypted file"
        }
        (crate::secrets::StoreKind::Files, Some(_)) => {
            "secret values are in files only your user can read (this computer has no credential store the engine could use)"
        }
        (crate::secrets::StoreKind::Files, None) => {
            "secret values are in files only your user can read"
        }
    };
    let mut envs = vec![json!({
        "id": "local",
        "name": "This computer",
        "kind": "local",
        "connected": true,
        "version": env!("CARGO_PKG_VERSION"),
        "endpoint": d.paths.endpoint().to_string(),
        "detail": format!(
            "Studio's own engine, profile {}. State in {}; {}.",
            d.paths.profile.as_deref().unwrap_or("default"),
            d.paths.state_dir.display(),
            secrets_detail
        ),
        "last_seen_at": ogun_types::Timestamp::now(),
    })];
    for e in edges.edges {
        envs.push(json!({
            "id": e.edge_id,
            "name": e.name,
            "kind": "edge",
            "connected": e.last_sync_at.is_some(),
            "detail": "an enrolled edge that syncs sealed windows to this engine",
            "last_seen_at": e.last_sync_at,
        }));
    }
    let bridges = json!([
        {"id": "zeus", "name": "Zeus session bridge", "state": if d.config.zeus_bridge.is_some() { "running" } else { "not_configured" },
         "detail": "Sources behind a browser sign-in go through zeusd as Zeus jobs; Ogun never reads browser sessions."},
        {"id": "ingest", "name": "Ingest bridge",
         "state": if ingest_recent(d) { "running" } else { "stopped" },
         "detail": "ogun-ingest reads MQTT, LoRaWAN, Modbus, file-tail and HTTP sources into stream windows (O-10). Studio starts it with the engine; it connects when a stream workflow is stored."},
        {"id": "sync", "name": "Edge sync", "state": "not_configured",
         "detail": "ogun-sync sends sealed windows from an edge to a hub (O-11)."},
        {"id": "server", "name": "Ogun server", "state": "not_configured",
         "detail": "ogun-server serves Studio-web, REST and MCP for a team; it is a separate product process."},
        {"id": "remote_approvals", "name": "Remote approvals", "state": "locked",
         "detail": "Passkey approvals from a browser stay off until the external security review ADR 0010 requires."},
    ]);
    Ok(json!({
        "environments": envs,
        "bridges": bridges,
        "scheduler_enabled": d.studio.runtime.scheduler_enabled(),
    }))
}

/// Routes `environments.*`, `sandbox.*` and `evidence.*`.
pub async fn dispatch(
    d: &Arc<Daemon>,
    _conn: &Conn,
    method: &str,
    p: Value,
) -> Option<ApiResult<Value>> {
    Some(match method {
        "environments.list" => environments(d),
        "sandbox.status" => out(&ogun_ops::sandbox::status().await),
        "evidence.get" => out(&d.studio.docs.read::<Evidence>("evidence")),
        "evidence.set" => args::<EvidenceSet>(p).and_then(|s| {
            let mut v = serde_json::to_value(d.studio.docs.read::<Evidence>("evidence"))
                .map_err(|e| ApiError::internal(e.to_string()))?;
            if let Some(obj) = v.as_object_mut() {
                for (k, x) in s.patch {
                    obj.insert(k, x);
                }
            }
            let next: Evidence = serde_json::from_value(v)
                .map_err(|e| ApiError::invalid_params(format!("evidence: {e}")))?;
            if !matches!(next.format.as_str(), "json" | "html") {
                return Err(ApiError::invalid_params("evidence: format is json or html"));
            }
            d.studio.docs.write("evidence", &next)?;
            out(&next)
        }),
        _ => return None,
    })
}
