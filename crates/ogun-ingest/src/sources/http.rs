//! `stream.http`: an HTTP push endpoint (O-10 section 2). It is the bridge's only listener, and
//! only when a workflow has an HTTP source.
//!
//! `POST /v1/streams/{workflow}/{node}/records` with `Authorization: Bearer <token>` and a body of
//! one JSON object, a JSON array of objects, NDJSON (`application/x-ndjson`), or CSV with a header
//! line (`text/csv`).
//!
//! | Status | When |
//! |---|---|
//! | `202` | The records committed; body `{"accepted": n, "dropped": d, "invalid": i}`. |
//! | `400` | The body does not decode, or it holds more than 10 000 records. |
//! | `401` | No or wrong bearer token. |
//! | `404` | Another path. |
//! | `413` | The body is larger than `max_body_bytes`. |
//! | `503` | The log is full under `block` (nothing was written); `Retry-After` in seconds. |
//!
//! Bind address (default `127.0.0.1:8740`), token file and body limit come from `ingest.json`.
//! Tokens are compared in constant time (BLAKE3 digests) and never logged.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;

use crate::applog::Reply;
use crate::config::{Secret, read_tokens};
use crate::decode::{csv_objects, json_objects, now_ms};
use crate::status::Counters;

use super::Ctx;

/// Default listen address.
pub const DEFAULT_BIND: &str = "127.0.0.1:8740";
/// Most records per request.
pub const MAX_RECORDS_PER_REQUEST: usize = 10_000;

struct App {
    ctx: Ctx,
    tokens: Vec<blake3::Hash>,
}

fn digest(token: &str) -> blake3::Hash {
    blake3::hash(token.as_bytes())
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let Some(given) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let given = digest(given.trim());
    // `blake3::Hash` equality is constant time; check every token so timing says nothing.
    app.tokens.iter().fold(false, |ok, t| ok | (*t == given))
}

fn json_body(status: StatusCode, body: &serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn records(
    State(app): State<Arc<App>>,
    Path((workflow, node)): Path<(String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = &app.ctx;
    if workflow != ctx.spec.workflow || node != ctx.spec.node {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !authorized(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "",
        )
            .into_response();
    }
    Counters::add(&ctx.counters.messages, 1);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (objects, bad) = if content_type.starts_with("text/csv") {
        csv_objects(&body, None)
    } else {
        json_objects(&body)
    };
    if objects.len() > MAX_RECORDS_PER_REQUEST {
        return json_body(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": "too many records in one request"}),
        );
    }
    if objects.is_empty() && bad > 0 {
        Counters::add(&ctx.counters.invalid, bad);
        return json_body(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": "the body holds no JSON object"}),
        );
    }
    let origin = format!("http:{}", peer.ip());
    let (records, invalid) = ctx.shaper.shape_all(objects, &origin, now_ms(), None);
    let invalid = invalid + bad;
    Counters::add(&ctx.counters.invalid, invalid);
    match ctx.log.append(records).await {
        Reply::Committed { written, dropped } => json_body(
            StatusCode::ACCEPTED,
            &serde_json::json!({"accepted": written, "dropped": dropped, "invalid": invalid}),
        ),
        Reply::Refused => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "5")],
            "",
        )
            .into_response(),
    }
}

/// Runs `stream.http`.
///
/// # Errors
///
/// A configuration error (no token file, bad bind address) or a bind failure.
pub async fn run(ctx: Ctx) -> Result<(), String> {
    let token_file = ctx
        .overrides
        .token_file
        .clone()
        .ok_or("stream.http needs token_file in ingest.json")?;
    let tokens: Vec<Secret> = read_tokens(&token_file, "token_file").map_err(|e| e.to_string())?;
    let tokens = tokens.iter().map(|t| digest(t.expose())).collect();
    let bind = ctx
        .overrides
        .bind
        .clone()
        .unwrap_or_else(|| DEFAULT_BIND.to_owned());
    let addr: SocketAddr = bind
        .parse()
        .map_err(|_| "bind must be an address and port, for example 127.0.0.1:8740")?;
    let max_body = ctx
        .overrides
        .max_body_bytes
        .unwrap_or(1024 * 1024)
        .min(16 * 1024 * 1024);
    let id = ctx.spec.id();
    let app = Arc::new(App { ctx, tokens });
    let router = Router::new()
        .route("/v1/streams/{workflow}/{node}/records", post(records))
        .layer(DefaultBodyLimit::max(
            usize::try_from(max_body).unwrap_or(usize::MAX),
        ))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {:?}", e.kind()))?;
    if !addr.ip().is_loopback() {
        tracing::warn!(source = %id, %addr, "HTTP push listens beyond loopback without TLS; put a TLS proxy in front or use a VPN");
    }
    tracing::info!(source = %id, %addr, "HTTP push listening");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|e| format!("HTTP push stopped: {:?}", e.kind()))
}
