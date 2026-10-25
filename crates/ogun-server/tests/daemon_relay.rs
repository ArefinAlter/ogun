//! End to end against a fake `ogund` on a Unix socket in a temporary directory: the bridge handshake with a
//! `bridge` credential, REST relays for sessions and tokens, the event stream's catch-up, and the full remote
//! approval path (passkey registration, decision options, a signed assertion, `approvals.decide` with WebAuthn
//! evidence). Hermetic: the only listener is the fake daemon's socket.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use ciborium::Value as Cbor;
use ogun_api::auth::{IpcSecret, Nonce, Token};
use ogun_api::jsonrpc::{Request as RpcRequest, Response as RpcResponse};
use ogun_api::methods::{AuthenticateParams, HelloParams};
use ogun_api::transport::Listener;
use ogun_api::{ApiError, ClientKind, Credential, OgunPaths};
use ogun_server::config::ServerConfig;
use ogun_server::principal::{SESSION_COOKIE, csrf_token, session_key};
use ogun_server::routes::{AppState, router};
use ogun_server::store::{Invite, Role, SessionRecord};
use ogun_server::util::{b64url, b64url_decode};
use ogun_types::{IdGenerator, Timestamp};
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::BufReader;
use tower::ServiceExt as _;

const ORIGIN: &str = "https://ogun.example.com";
const RP: &str = "ogun.example.com";

#[derive(Default)]
struct Fake {
    approvers: Vec<Value>,
    decided: Vec<Value>,
}

fn now() -> String {
    Timestamp::now().to_rfc3339()
}

fn approval(id: &str) -> Value {
    json!({
        "approval_id": id, "kind": "code_change", "state": "pending", "workflow": "w",
        "workflow_hash": "h", "facts": [], "request_hash": "ab".repeat(32),
        "requested_by": "cli (human)", "created_at": now(), "expires_at": now()
    })
}

async fn serve_conn(
    stream: ogun_api::transport::Stream,
    secret: Arc<IpcSecret>,
    token: Arc<Token>,
    fake: Arc<Mutex<Fake>>,
) {
    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);
    let run_id = IdGenerator::new().run_id();
    let mut nonces: Option<(Nonce, Nonce)> = None;
    while let Ok(Some(req)) = ogun_api::framing::read_message::<RpcRequest>(&mut r, 1 << 20).await {
        let p = req.params.clone();
        let result: Result<Value, ApiError> = match req.method.as_str() {
            "daemon.hello" => {
                let h: HelloParams = serde_json::from_value(p).unwrap();
                let sn = Nonce::generate().unwrap();
                let proof = secret.server_proof(&h.client_nonce);
                nonces = Some((sn.clone(), h.client_nonce));
                Ok(
                    json!({ "protocol": 1, "daemon_version": "fake", "server_nonce": sn, "server_proof": proof }),
                )
            }
            "daemon.authenticate" => {
                let a: AuthenticateParams = serde_json::from_value(p).unwrap();
                let (sn, cn) = nonces.clone().unwrap();
                if token.auth_key().verify(a.credential_id, &sn, &cn, &a.proof) {
                    Ok(
                        json!({ "kind": "bridge", "name": "server", "interactive": false,
                        "methods": ogun_api::policy::methods_for(ClientKind::Bridge, false) }),
                    )
                } else {
                    Err(ApiError::new("api.unauthenticated", "bad proof"))
                }
            }
            "daemon.status" => Ok(
                json!({ "daemon_version": "fake", "started_at": now(), "workflows": 0,
                "runs_active": 0, "approvals_pending": 1, "streams": 0, "last_event_seq": 2, "scheduler_enabled": true }),
            ),
            "workflows.list" => Ok(json!({ "workflows": [] })),
            "events.since" => {
                let after = p["after"].as_u64().unwrap_or(0);
                if after < 2 {
                    let evs: Vec<Value> = ((after + 1)..=2)
                        .map(|s| json!({ "seq": s, "at": now(), "run_id": run_id, "kind": { "type": "test" } }))
                        .collect();
                    Ok(json!({ "events": evs, "last_seq": 2 }))
                } else {
                    if p["wait_ms"].as_u64().unwrap_or(0) > 0 {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    Ok(json!({ "events": [], "last_seq": 2 }))
                }
            }
            "approvals.get" => Ok(approval(p["approval_id"].as_str().unwrap())),
            "approvers.propose" => {
                let key = b64url_decode(p["public_key"].as_str().unwrap(), "k").unwrap();
                let v = json!({ "credential_id": p["credential_id"], "user": p["user"], "rp_id": p["rp_id"],
                    "fingerprint": ogun_server::webauthn::fingerprint(&key), "confirmed": false, "created_at": now() });
                let mut confirmed = v.clone();
                // The host person confirms it at once in this test.
                confirmed["confirmed"] = json!(true);
                fake.lock().unwrap().approvers.push(confirmed);
                Ok(v)
            }
            "approvers.list" => Ok(json!({ "approvers": fake.lock().unwrap().approvers.clone() })),
            "approvals.decide" => {
                fake.lock().unwrap().decided.push(p.clone());
                let mut v = approval(p["approval_id"].as_str().unwrap());
                v["state"] = json!(if p["decision"] == "approve" {
                    "approved"
                } else {
                    "denied"
                });
                v["decided_via"] = json!("webauthn");
                Ok(v)
            }
            other => Err(ApiError::new(
                "api.method_not_found",
                format!("fake has no {other}"),
            )),
        };
        let resp = match result {
            Ok(v) => RpcResponse::ok(req.id, v),
            Err(e) => RpcResponse::err(Some(req.id), e),
        };
        if ogun_api::framing::write_message(&mut w, &resp)
            .await
            .is_err()
        {
            return;
        }
    }
}

struct World {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    fake: Arc<Mutex<Fake>>,
    cookie: String,
    csrf: String,
    user: String,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let paths = OgunPaths::rooted(&dir.path().join("ogun"), Some("server"));
    std::fs::create_dir_all(paths.credentials_dir()).unwrap();
    let secret = IpcSecret::generate().unwrap();
    std::fs::write(paths.ipc_secret_file(), secret.to_file_text().as_bytes()).unwrap();
    let cred = Credential {
        id: IdGenerator::new().credential_id(),
        kind: ClientKind::Bridge,
        name: "server".into(),
        token: Token::generate().unwrap(),
    };
    std::fs::write(
        paths.credential_file("server"),
        cred.to_file_text().as_bytes(),
    )
    .unwrap();
    let token = Arc::new(Token::parse(&cred.token.to_text()).unwrap());
    let secret = Arc::new(secret);
    let fake = Arc::new(Mutex::new(Fake::default()));
    let mut listener = Listener::bind(&paths.endpoint()).await.unwrap();
    let f2 = Arc::clone(&fake);
    tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            tokio::spawn(serve_conn(
                stream,
                Arc::clone(&secret),
                Arc::clone(&token),
                Arc::clone(&f2),
            ));
        }
    });

    let cfg: ServerConfig = serde_json::from_value(json!({
        "public_url": ORIGIN,
        "listen": "127.0.0.1:0",
        "state_dir": dir.path().join("server"),
        "remote_approvals": true,
        "daemons": { "default": { "ogun_home": dir.path().join("ogun"), "profile": "server" } }
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
        .accept_invite("k", "https://idp", "alice", "Alice", None)
        .unwrap();
    let secret = "relay-session-secret-00000000000000000000000";
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
    World {
        _dir: dir,
        state,
        fake,
        cookie: format!("{SESSION_COOKIE}={secret}"),
        csrf: csrf_token(secret),
        user: user.user_id.to_string(),
    }
}

impl World {
    fn get(&self, path: &str) -> Request<Body> {
        Request::get(path)
            .header(header::HOST, RP)
            .header(header::COOKIE, &self.cookie)
            .body(Body::empty())
            .unwrap()
    }

    fn post(&self, path: &str, body: &Value) -> Request<Body> {
        Request::post(path)
            .header(header::HOST, RP)
            .header(header::COOKIE, &self.cookie)
            .header("x-ogun-csrf", &self.csrf)
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = router(Arc::clone(&self.state)).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

fn sha256(b: &[u8]) -> Vec<u8> {
    Sha256::digest(b).to_vec()
}

#[tokio::test]
async fn relays_reads_for_sessions_and_tokens() {
    let w = world().await;
    let (s, v) = w.send(w.get("/api/v1/status")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["daemon_version"], "fake");
    let (s, v) = w
        .send(w.post("/api/v1/tokens", &json!({ "name": "agent" })))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bearer = format!("Bearer {}", v["secret"].as_str().unwrap());
    let req = Request::get("/api/v1/workflows")
        .header(header::HOST, RP)
        .header(header::AUTHORIZATION, bearer)
        .body(Body::empty())
        .unwrap();
    let (s, v) = w.send(req).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["workflows"], json!([]));
}

#[tokio::test]
async fn the_event_stream_catches_up_from_after() {
    let w = world().await;
    let resp = router(Arc::clone(&w.state))
        .oneshot(w.get("/api/v1/events?after=0"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let mut body = resp.into_body().into_data_stream();
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    use futures::StreamExt as _;
    while !text.contains("id: 2") && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), body.next()).await {
            Ok(Some(Ok(chunk))) => text.push_str(&String::from_utf8_lossy(&chunk)),
            _ => break,
        }
    }
    assert!(text.contains("event: ogun"), "{text}");
    assert!(text.contains("id: 1") && text.contains("id: 2"), "{text}");
}

#[tokio::test]
async fn a_remote_approval_travels_with_webauthn_evidence() {
    let w = world().await;
    let sk = SigningKey::from_slice(&[11u8; 32]).unwrap();
    let vk = *sk.verifying_key();
    let point = vk.to_sec1_point(false);

    // Register a passkey.
    let (s, opts) = w
        .send(w.post("/api/v1/passkeys/register-options", &json!({})))
        .await;
    assert_eq!(s, StatusCode::OK, "{opts}");
    let challenge = b64url_decode(opts["challenge"].as_str().unwrap(), "c").unwrap();
    let cred_id = b"passkey-1";
    let cose = Cbor::Map(vec![
        (Cbor::Integer(1.into()), Cbor::Integer(2.into())),
        (Cbor::Integer(3.into()), Cbor::Integer((-7).into())),
        (Cbor::Integer((-1).into()), Cbor::Integer(1.into())),
        (
            Cbor::Integer((-2).into()),
            Cbor::Bytes(point.x().unwrap().to_vec()),
        ),
        (
            Cbor::Integer((-3).into()),
            Cbor::Bytes(point.y().unwrap().to_vec()),
        ),
    ]);
    let mut auth = sha256(RP.as_bytes());
    auth.push(0x01 | 0x04 | 0x40);
    auth.extend_from_slice(&0u32.to_be_bytes());
    auth.extend_from_slice(&[0u8; 16]);
    auth.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
    auth.extend_from_slice(cred_id);
    ciborium::ser::into_writer(&cose, &mut auth).unwrap();
    let cdata =
        json!({ "type": "webauthn.create", "challenge": b64url(&challenge), "origin": ORIGIN })
            .to_string();
    let att = Cbor::Map(vec![
        (Cbor::Text("fmt".into()), Cbor::Text("none".into())),
        (Cbor::Text("attStmt".into()), Cbor::Map(vec![])),
        (Cbor::Text("authData".into()), Cbor::Bytes(auth)),
    ]);
    let mut att_bytes = Vec::new();
    ciborium::ser::into_writer(&att, &mut att_bytes).unwrap();
    let (s, reg) = w
        .send(w.post(
            "/api/v1/passkeys/register",
            &json!({
                "credential_id": b64url(cred_id),
                "client_data_json": b64url(cdata.as_bytes()),
                "attestation_object": b64url(&att_bytes),
            }),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{reg}");
    assert!(
        reg["confirm_command"]
            .as_str()
            .unwrap()
            .starts_with("ogun --profile server approvers confirm")
    );

    // Ask for decision options and sign them.
    let approval_id = IdGenerator::new().approval_id().to_string();
    let (s, opts) = w
        .send(w.post(
            &format!("/api/v1/approvals/{approval_id}/webauthn-options"),
            &json!({ "decision": "approve" }),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{opts}");
    assert_eq!(opts["allowCredentials"][0]["id"], b64url(cred_id));
    let challenge = b64url_decode(opts["challenge"].as_str().unwrap(), "c").unwrap();
    let mut adata = sha256(RP.as_bytes());
    adata.push(0x01 | 0x04);
    adata.extend_from_slice(&1u32.to_be_bytes());
    let cd = json!({ "type": "webauthn.get", "challenge": b64url(&challenge), "origin": ORIGIN })
        .to_string();
    let mut msg = adata.clone();
    msg.extend_from_slice(&sha256(cd.as_bytes()));
    let sig: Signature = sk.sign(&msg);
    let decide = json!({
        "decision": "approve",
        "request_hash": "ab".repeat(32),
        "assertion": {
            "credential_id": b64url(cred_id),
            "client_data_json": b64url(cd.as_bytes()),
            "authenticator_data": b64url(&adata),
            "signature": b64url(sig.to_der().as_bytes()),
        }
    });
    let (s, v) = w
        .send(w.post(&format!("/api/v1/approvals/{approval_id}/decide"), &decide))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "approved");
    let decided = w.fake.lock().unwrap().decided.clone();
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0]["evidence"]["channel"], "webauthn");
    assert_eq!(decided[0]["evidence"]["user"], w.user.as_str());

    // The same assertion again fails: the signature counter did not increase.
    let (s, _) = w
        .send(w.post(&format!("/api/v1/approvals/{approval_id}/decide"), &decide))
        .await;
    assert_ne!(s, StatusCode::OK);
    // A deny decision with the approve assertion fails: the challenge binds the decision.
    let mut deny = decide.clone();
    deny["decision"] = json!("deny");
    let (s, _) = w
        .send(w.post(&format!("/api/v1/approvals/{approval_id}/decide"), &deny))
        .await;
    assert_ne!(s, StatusCode::OK);
    assert_eq!(w.fake.lock().unwrap().decided.len(), 1);
}
