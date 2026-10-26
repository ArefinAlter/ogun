//! Native sign-in (O-13 section 4.3): an ID token that a desktop application obtained from the identity provider
//! with authorization code and PKCE is exchanged at `POST /auth/native` for the hub's own session, presented as a
//! bearer. The identity provider here is a real HTTP server on loopback that serves discovery and a key set and
//! signs ES256 ID tokens with a throwaway test key; nothing leaves the machine.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ogun_server::config::ServerConfig;
use ogun_server::routes::{AppState, router};
use ogun_server::store::{Invite, Role};
use ogun_types::Timestamp;
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// A throwaway P-256 key made for these tests. It signs nothing outside them.
const TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgyudtmrNzJD69ECnH
/CkzO/NjMPyy9cdeQIX3teX7Z4yhRANCAAQbbwIhahZDbPKqWyX8NKCGys5sSI3x
TyIib4+t48apj4PIulsWuCSsujzC0g2oVAupxjCgrvQpP+VcL45SCyxl
-----END PRIVATE KEY-----
";
const TEST_KEY_X: &str = "G28CIWoWQ2zyqlsl_DSghsrObEiN8U8iIm-PrePGqY8";
const TEST_KEY_Y: &str = "g8i6Wxa4JKy6PMLSDahUC6nGMKCu9Ck_5VwvjlILLGU";
const NONCE: &str = "nonce-0123456789abcdef0123456789";

struct Idp {
    base: String,
}

async fn start_idp() -> Idp {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let issuer = base.clone();
    let doc = json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{base}/auth"),
        "token_endpoint": format!("{base}/token"),
        "jwks_uri": format!("{base}/keys"),
        "code_challenge_methods_supported": ["S256"],
    });
    let keys = json!({ "keys": [{
        "kty": "EC", "crv": "P-256", "kid": "k1", "alg": "ES256", "use": "sig",
        "x": TEST_KEY_X, "y": TEST_KEY_Y,
    }]});
    let app = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let d = doc.clone();
                async move { axum::Json(d) }
            }),
        )
        .route(
            "/keys",
            get(move || {
                let k = keys.clone();
                async move { axum::Json(k) }
            }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Idp { base }
}

fn id_token(idp: &Idp, aud: &str, sub: &str, nonce: &str, age_s: i64) -> String {
    let now = Timestamp::now().unix_millis() / 1000;
    let claims = json!({
        "iss": idp.base, "sub": sub, "aud": aud,
        "iat": now - age_s, "exp": now - age_s + 3600,
        "email": "alice@example.com", "email_verified": true, "name": "Alice",
    });
    let mut claims = claims;
    // An empty nonce means the token carries none, as a token the provider issues on a refresh.
    if !nonce.is_empty() {
        claims["nonce"] = json!(nonce);
    }
    let body: String = TEST_KEY_PEM
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body).unwrap();
    let mut h = Header::new(Algorithm::ES256);
    h.kid = Some("k1".into());
    jsonwebtoken::encode(&h, &claims, &EncodingKey::from_ec_der(&der)).unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    idp: Idp,
}

async fn fixture(native_client: Option<&str>) -> Fixture {
    let idp = start_idp().await;
    let dir = tempfile::tempdir().unwrap();
    let mut provider = json!({
        "id": "dex", "display_name": "Sample directory", "issuer": idp.base,
        "client_id": "ogun-web", "algorithms": ["ES256"],
    });
    if let Some(n) = native_client {
        provider["native_client_id"] = json!(n);
    }
    let cfg: ServerConfig = serde_json::from_value(json!({
        "public_url": "https://ogun.example.com",
        "listen": "127.0.0.1:0",
        "state_dir": dir.path().join("server"),
        "daemons": { "default": { "ogun_home": dir.path().join("ogun"), "profile": null } },
        "oidc": { "providers": [provider] },
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
                role: Role::Editor,
                approver: false,
                created_at: Timestamp::now(),
                expires_at: Timestamp::now()
                    .checked_add(Duration::from_secs(60))
                    .unwrap(),
                created_by: None,
            },
        )
        .unwrap();
    state
        .store
        .accept_invite("k", &idp.base, "alice", "Alice", None)
        .unwrap();
    Fixture {
        _dir: dir,
        state,
        idp,
    }
}

async fn send(f: &Fixture, req: Request<Body>) -> (StatusCode, Value) {
    let resp = router(Arc::clone(&f.state)).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn native_request(token: &str, nonce: &str) -> Request<Body> {
    Request::post("/auth/native")
        .header(header::HOST, "ogun.example.com")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            if nonce.is_empty() {
                json!({ "provider": "dex", "id_token": token })
            } else {
                json!({ "provider": "dex", "id_token": token, "nonce": nonce })
            }
            .to_string(),
        ))
        .unwrap()
}

fn bearer(path: &str, token: &str) -> axum::http::request::Builder {
    Request::get(path)
        .header(header::HOST, "ogun.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
}

#[tokio::test]
async fn an_id_token_from_the_native_client_becomes_a_person_session() {
    let f = fixture(Some("ogun-studio")).await;
    let token = id_token(&f.idp, "ogun-studio", "alice", NONCE, 5);
    let (status, v) = send(&f, native_request(&token, NONCE)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let session = v["token"].as_str().unwrap().to_owned();
    assert!(session.starts_with("ogs1_"));
    assert_eq!(v["user"]["display_name"], "Alice");
    assert!(v["expires_in_s"].as_u64().unwrap() > 0);
    // The ID token is not the credential: the session is.
    assert!(!session.contains(&token));

    // The session acts as the person, with the role the workspace gave them.
    let (status, me) = send(
        &f,
        bearer("/api/v1/me", &session).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me.to_string().matches("editor").count() >= 1, true, "{me}");

    // A person's session may create an API token (an agent's credential); an API token never may. No CSRF header
    // is needed for a bearer.
    let (status, minted) = send(
        &f,
        Request::post("/api/v1/tokens")
            .header(header::HOST, "ogun.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {session}"))
            .body(Body::from(
                json!({ "name": "ci", "role": "viewer" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{minted}");

    // Signing out ends it on the hub.
    let (status, _) = send(
        &f,
        Request::post("/auth/logout")
            .header(header::HOST, "ogun.example.com")
            .header(header::AUTHORIZATION, format!("Bearer {session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = send(
        &f,
        bearer("/api/v1/me", &session).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
}

#[tokio::test]
async fn the_browser_client_id_is_accepted_too_but_other_audiences_are_not() {
    let f = fixture(Some("ogun-studio")).await;
    let ok = id_token(&f.idp, "ogun-web", "alice", NONCE, 5);
    let (status, v) = send(&f, native_request(&ok, NONCE)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let other = id_token(&f.idp, "some-other-client", "alice", NONCE, 5);
    let (status, v) = send(&f, native_request(&other, NONCE)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"]["code"], "server.sign_in_failed");
}

#[tokio::test]
async fn the_nonce_the_age_the_account_and_the_provider_setting_are_all_checked() {
    let f = fixture(Some("ogun-studio")).await;
    // A nonce the app did not send.
    let t = id_token(&f.idp, "ogun-studio", "alice", NONCE, 5);
    let (status, _) = send(&f, native_request(&t, "a-different-nonce-0123456789abcdef")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A stolen token replayed after ten minutes.
    let old = id_token(&f.idp, "ogun-studio", "alice", NONCE, 900);
    let (status, v) = send(&f, native_request(&old, NONCE)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    // A person with no account: there is no sign-up on this route.
    let stranger = id_token(&f.idp, "ogun-studio", "mallory", NONCE, 5);
    let (status, v) = send(&f, native_request(&stranger, NONCE)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
    // A garbled token never reaches a signature check.
    let (status, _) = send(&f, native_request("not.a.token", NONCE)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Nothing was created by any of that.
    assert!(
        f.state
            .store
            .user_by_identity(&f.idp.base, "mallory")
            .unwrap()
            .is_none()
    );

    // A hub whose provider has no native client refuses the route outright.
    let g = fixture(None).await;
    let t = id_token(&g.idp, "ogun-web", "alice", NONCE, 5);
    let (status, v) = send(&g, native_request(&t, NONCE)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
}

#[tokio::test]
async fn an_id_token_is_never_a_bearer_for_the_api() {
    let f = fixture(Some("ogun-studio")).await;
    let t = id_token(&f.idp, "ogun-studio", "alice", NONCE, 5);
    let (status, v) = send(&f, bearer("/api/v1/me", &t).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{v}");
}

#[tokio::test]
async fn a_refreshed_id_token_has_no_nonce_and_may_say_so_only_then() {
    let f = fixture(Some("ogun-studio")).await;
    // The provider's refresh answer carries no nonce; the app sends none.
    let refreshed = id_token(&f.idp, "ogun-studio", "alice", "", 5);
    let (status, v) = send(&f, native_request(&refreshed, "")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // A token with a nonce cannot be exchanged without it, and a nonce-less one not with an invented nonce.
    let with = id_token(&f.idp, "ogun-studio", "alice", NONCE, 5);
    let (status, _) = send(&f, native_request(&with, "")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&f, native_request(&refreshed, NONCE)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_providers_route_tells_a_desktop_app_what_it_needs_and_nothing_secret() {
    let f = fixture(Some("ogun-studio")).await;
    let (status, v) = send(
        &f,
        Request::get("/auth/providers")
            .header(header::HOST, "ogun.example.com")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["native"][0]["client_id"], "ogun-studio");
    assert_eq!(v["native"][0]["issuer"], f.idp.base.as_str());
    assert_eq!(v["native"][0]["id"], "dex");
    let g = fixture(None).await;
    let (_, v) = send(
        &g,
        Request::get("/auth/providers")
            .header(header::HOST, "ogun.example.com")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(v["native"].as_array().unwrap().len(), 0);
}
