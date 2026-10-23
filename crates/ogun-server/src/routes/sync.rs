//! The edge sync relay (O-13 section 9, O-11): `POST /api/v1/sync/offer` and `POST /api/v1/sync/chunk`.
//!
//! These routes carry no session or token. The edge's Ed25519 signature on the offer is the authentication and
//! the daemon verifies it, as it verifies every completed object's digest; the bridge only checks the shape,
//! limits the size and rate-limits per address. They are off unless `sync.enabled` is set.

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use ogun_api::methods::{SyncChunkParams, SyncOfferParams, name};

use crate::error::{ServerError, ServerResult};
use crate::ratelimit::Class;
use crate::routes::api::{St, body};
use crate::security::ClientIp;

/// Largest base64 payload of one chunk (O-15 section 5: 768 KiB per chunk on this transport, base64 included).
const MAX_CHUNK_BASE64: usize = 1024 * 1024;

async fn relay(
    s: &crate::routes::AppState,
    ip: ClientIp,
    method: &str,
    params: serde_json::Value,
) -> ServerResult<serde_json::Value> {
    if !s.cfg.sync.enabled {
        return Err(ServerError::not_found(
            "edge sync is not enabled on this server",
        ));
    }
    s.limiter.check_ip(Class::Sync, ip.0)?;
    let pool = s
        .daemons
        .get(&s.cfg.sync.daemon)
        .ok_or_else(|| ServerError::unavailable("the sync daemon is not configured"))?;
    let r = pool.call_value(method, params).await;
    if let Err(e) = &r {
        tracing::info!(client = %ip.0, method, code = %e.code(), "sync refused");
    }
    r
}

/// `POST /api/v1/sync/offer`.
pub async fn offer(State(s): St, Extension(ip): Extension<ClientIp>, b: Bytes) -> Response {
    let r = async {
        let p: SyncOfferParams = body(&b)?;
        relay(
            &s,
            ip,
            name::SYNC_OFFER,
            serde_json::to_value(&p).map_err(|e| ServerError::internal(e.to_string()))?,
        )
        .await
    }
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /api/v1/sync/chunk`.
pub async fn chunk(State(s): St, Extension(ip): Extension<ClientIp>, b: Bytes) -> Response {
    let r = async {
        let p: SyncChunkParams = body(&b)?;
        if p.data_base64.len() > MAX_CHUNK_BASE64 {
            return Err(ServerError::new(
                "api.payload_too_large",
                "a sync chunk is at most 1 MiB of base64",
            ));
        }
        if p.total == 0 || p.index >= p.total || !crate::util::is_blake3_hex(&p.blake3) {
            return Err(ServerError::invalid(
                "chunk index, total or digest is malformed",
            ));
        }
        relay(
            &s,
            ip,
            name::SYNC_CHUNK,
            serde_json::to_value(&p).map_err(|e| ServerError::internal(e.to_string()))?,
        )
        .await
    }
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}
