//! Router-level tests without a daemon: authentication, CSRF, the agent ceiling of API tokens, the Host check,
//! security headers and MCP's token-only rule. Hermetic: no socket is opened.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ogun_server::config::ServerConfig;
use ogun_server::principal::{SESSION_COOKIE, csrf_token, session_key};
use ogun_server::routes::{AppState, router};
use ogun_server::store::{Invite, Role, SessionRecord};
use ogun_types::Timestamp;
use serde_json::{Value, json};
use tower::ServiceExt as _;

struct Fixture {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    cookie: String,
    csrf: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let cfg: ServerConfig = serde_json::from_value(json!({
        "public_url": "https://ogun.example.com",
        "listen": "127.0.0.1:0",
        "state_dir": dir.path().join("server"),
        "daemons": { "default": { "ogun_home": dir.path().join("ogun"), "profile": null } }
    }))
    .unwrap();
    let state = ogun_server::build_state(cfg).unwrap();
    let w = state.store.create_workspace("main", "default").unwrap();
    state
        .store
        .put_invite(
            "k",
            &Invite {
                workspace_id: w.workspace_id,
                role: Role::Owner,
                approver: true,
                created_at: Timestamp::now(),
                expires_at: Timestamp::now()
                    .checked_add(Duration::from_secs(60))
                    .unwrap(),
                created_by: None,
            },
        )
        .unwrap();
    let (user, _) = state
        .store
        .accept_invite("k", "https://idp.example.com", "alice", "Alice", None)
        .unwrap();
    let secret = "test-session-secret-0000000000000000000000";
    state
        .store
        .put_session(
            &session_key(secret),
            &SessionRecord {
                user_id: user.user_id,
                provider: "idp".into(),
                created_at: Timestamp::now(),
                last_seen_at: Timestamp::now(),
            },
        )
        .unwrap();
    Fixture {
        _dir: dir,
        state,
        cookie: format!("{SESSION_COOKIE}={secret}"),
        csrf: csrf_token(secret),
    }
}

async fn send(f: &Fixture, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = router(Arc::clone(&f.state)).oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, v)
}

fn get(path: &str) -> axum::http::request::Builder {
    Request::get(path).header(header::HOST, "ogun.example.com")
}

fn post(path: &str) -> axum::http::request::Builder {
    Request::post(path)
        .header(header::HOST, "ogun.example.com")
        .header(header::CONTENT_TYPE, "application/json")
}

async fn mint_token(f: &Fixture, role: &str) -> String {
    let (status, _, v) = send(
        f,
        post("/api/v1/tokens")
            .header(header::COOKIE, &f.cookie)
            .header("x-ogun-csrf", &f.csrf)
            .header(header::ORIGIN, "https://ogun.example.com")
            .body(Body::from(
                json!({ "name": "ci", "role": role }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["secret"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn unauthenticated_requests_get_an_envelope_and_strict_headers() {
    let f = fixture();
    let (status, h, v) = send(&f, get("/api/v1/status").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "api.unauthenticated");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert!(
        h["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    assert_eq!(h["cache-control"], "no-store");
    assert!(h.contains_key("strict-transport-security"));
}

#[tokio::test]
async fn a_foreign_host_header_is_refused() {
    let f = fixture();
    let req = Request::get("/api/v1/me")
        .header(header::HOST, "evil.example")
        .header(header::COOKIE, &f.cookie)
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&f, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn sessions_need_the_csrf_header_on_writes() {
    let f = fixture();
    let (status, _, me) = send(
        &f,
        get("/api/v1/me")
            .header(header::COOKIE, &f.cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["csrf_token"], f.csrf.as_str());
    assert_eq!(me["workspaces"][0]["role"], "owner");
    let (status, _, v) = send(
        &f,
        post("/api/v1/tokens")
            .header(header::COOKIE, &f.cookie)
            .body(Body::from(json!({ "name": "x" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "server.csrf_failed");
    let (status, _, _) = send(
        &f,
        post("/api/v1/tokens")
            .header(header::COOKIE, &f.cookie)
            .header("x-ogun-csrf", &f.csrf)
            .header(header::ORIGIN, "https://evil.example")
            .body(Body::from(json!({ "name": "x" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn api_tokens_act_as_agents_and_never_reach_approval_routes() {
    let f = fixture();
    let token = mint_token(&f, "editor").await;
    let bearer = format!("Bearer {token}");
    let (status, _, me) = send(
        &f,
        get("/api/v1/me")
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["acts_as"], "agent");
    for path in [
        "/api/v1/passkeys/register-options",
        "/api/v1/approvals/oap_01J00000000000000000000000/webauthn-options",
    ] {
        let (status, _, v) = send(
            &f,
            post(path)
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::from(json!({ "decision": "approve" }).to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {v}");
    }
    // Pausing a workflow is not an agent method (and needs admin).
    let (status, _, _) = send(
        &f,
        post("/api/v1/workflows/w/enabled")
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::from(json!({ "enabled": false }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Tokens cannot mint tokens.
    let (status, _, _) = send(
        &f,
        post("/api/v1/tokens")
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::from(json!({ "name": "y" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Reads go to the daemon, which is not running here.
    let (status, _, v) = send(
        &f,
        get("/api/v1/status")
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    // A wrong secret is refused.
    let flipped = if token.ends_with('0') { '1' } else { '0' };
    let forged = format!("Bearer {}{flipped}", &token[..token.len() - 1]);
    let (status, _, _) = send(
        &f,
        get("/api/v1/me")
            .header(header::AUTHORIZATION, forged)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn remote_approvals_are_off_by_default() {
    let f = fixture();
    let body = json!({
        "decision": "approve",
        "request_hash": "ab".repeat(32),
        "assertion": { "credential_id": "x", "client_data_json": "x", "authenticator_data": "x", "signature": "x" }
    });
    let (status, _, v) = send(
        &f,
        post("/api/v1/approvals/oap_01J00000000000000000000000/decide")
            .header(header::COOKIE, &f.cookie)
            .header("x-ogun-csrf", &f.csrf)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "server.remote_approvals_disabled");
}

#[tokio::test]
async fn mcp_is_for_tokens_only_and_lists_no_deciding_tool() {
    let f = fixture();
    let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string();
    let (status, _, _) = send(
        &f,
        post("/mcp")
            .header(header::COOKIE, &f.cookie)
            .header("x-ogun-csrf", &f.csrf)
            .body(Body::from(msg.clone()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let token = mint_token(&f, "viewer").await;
    let (status, _, v) = send(
        &f,
        post("/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::from(msg))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tools = v["result"]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .all(|t| !t["name"].as_str().unwrap().contains("decide"))
    );
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string();
    let (status, _, _) = send(
        &f,
        post("/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(note))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn the_built_in_page_is_served_without_studio_web() {
    let f = fixture();
    let resp = router(Arc::clone(&f.state))
        .oneshot(get("/runs/anything").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
}

#[tokio::test]
async fn sync_chunks_are_capped_before_reaching_the_daemon() {
    let f = fixture();
    let request = json!({
        "edge_id": "edg_01M45MN5M0PZ0D9W0APDCGRNZD",
        "partition": "w20261023T081500Z",
        "blake3": "a".repeat(64),
        "index": 0,
        "total": 1,
        "data_base64": "A".repeat((512 * 1024usize).div_ceil(3) * 4 + 1)
    });
    let (status, _, value) = send(
        &f,
        post("/api/v1/sync/chunk")
            .body(Body::from(request.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(value["error"]["code"], "api.payload_too_large");
}
