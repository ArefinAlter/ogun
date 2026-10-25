//! The bridge against a fake `zeusd` on a loopback Unix socket: handshake, root registration, submission, status
//! and import into a real `zeus-artifact` store. Hermetic: a temporary directory and one socket.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ogun_zeus_bridge::{BridgeConfig, BridgeError, JobStatus, SubmitOptions, ZeusBridge};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

const SECRET: [u8; 32] = [7u8; 32];
const TOKEN_HEX: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";
const CRED_ID: &str = "cli_01M47A2B3C4D5E6F7G8H9J0K1M";
const CONTENT: &[u8] = b"%PDF-1.7 annual report";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    HumanCredential,
    WrongProof,
    DropEnqueueResponse,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn token_bytes() -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, c) in TOKEN_HEX.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap();
    }
    out
}

fn setup(dir: &Path) -> (BridgeConfig, PathBuf, PathBuf) {
    let state = dir.join("zeus-state");
    std::fs::create_dir_all(state.join("credentials")).unwrap();
    std::fs::write(state.join("ipc.secret"), format!("{}\n", hex(&SECRET))).unwrap();
    let cred = state.join("credentials").join("ogun.token");
    std::fs::write(&cred, format!("{CRED_ID} zt1_{TOKEN_HEX}\n")).unwrap();
    let sock_dir = dir.join("run").join("zeus");
    std::fs::create_dir_all(&sock_dir).unwrap();
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sock = sock_dir.join("zeusd.sock");
    let root = dir.join("Downloads").join("Zeus").join("ogun");
    let mut cfg = BridgeConfig::new(cred, root.clone());
    cfg.endpoint = Some(sock.to_str().unwrap().to_owned());
    (cfg, sock, root)
}

async fn serve(listener: UnixListener, mode: Mode) {
    serve_with_count(listener, mode, Arc::new(AtomicUsize::new(0))).await;
}

async fn serve_with_count(listener: UnixListener, mode: Mode, enqueues: Arc<AtomicUsize>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let enqueues = enqueues.clone();
        tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(stream);
            let mut lines = BufReader::new(r).lines();
            let mut client_nonce = String::new();
            let server_nonce = hex(&[9u8; 32]);
            let mut file: Option<PathBuf> = None;
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].clone();
                let p = &req["params"];
                let result = match req["method"].as_str().unwrap() {
                    "daemon.hello" => {
                        client_nonce = p["client_nonce"].as_str().unwrap().to_owned();
                        let mut h = blake3::Hasher::new_keyed(&SECRET);
                        h.update(b"zeus-server-v1");
                        h.update(client_nonce.as_bytes());
                        let mut proof = h.finalize().to_hex().to_string();
                        if mode == Mode::WrongProof {
                            proof = "00".repeat(32);
                        }
                        json!({"protocol_version":1,"capabilities":["events","jobs.wait","idempotency","files.roots"],
                               "limits":{"max_request_bytes":1048576,"max_response_bytes":16777216,"max_in_flight_requests":32,"max_subscriptions":4,"event_buffer":1024},
                               "daemon":{"version":"0.1.0","instance_id":"3f9a6c2d1b7e4a08c5d2e9f1a0b3c4d5"},
                               "server_nonce": server_nonce, "server_proof": proof})
                    }
                    "daemon.authenticate" => {
                        let key =
                            blake3::derive_key("zeus 2026-10 client auth key v1", &token_bytes());
                        let mut h = blake3::Hasher::new_keyed(&key);
                        h.update(b"zeus-client-v1");
                        h.update(CRED_ID.as_bytes());
                        h.update(server_nonce.as_bytes());
                        h.update(client_nonce.as_bytes());
                        assert_eq!(p["proof"].as_str().unwrap(), h.finalize().to_hex().as_str());
                        let kind = if mode == Mode::HumanCredential {
                            "human"
                        } else {
                            "service"
                        };
                        json!({"client_id":CRED_ID,"kind":kind,"credential_kind":kind,"downgraded":false,"role":"service",
                               "permissions":["jobs.read","jobs.write","files.roots.register"],
                               "limits":{"max_request_bytes":1048576,"max_response_bytes":16777216,"max_in_flight_requests":32,"max_subscriptions":4,"event_buffer":1024},
                               "connection_id":"conn_01M45P0Z9X8W7V6T5S4R3Q2P1N"})
                    }
                    "files.roots.register" => {
                        let path = p["path"].as_str().unwrap();
                        std::fs::create_dir_all(path).unwrap();
                        json!({"root":{"path":path,"label":p["label"],"client_id":CRED_ID,"created_at":"2026-10-06T08:00:00.000Z"},"created":true,"replayed":false})
                    }
                    "jobs.enqueue" => {
                        let job = &p["jobs"][0];
                        assert!(job.get("cookie").is_none());
                        let dest = PathBuf::from(job["dest_dir"].as_str().unwrap());
                        let f = dest.join("annual-report.pdf");
                        std::fs::write(&f, CONTENT).unwrap();
                        file = Some(f);
                        let attempt = enqueues.fetch_add(1, Ordering::SeqCst);
                        if mode == Mode::DropEnqueueResponse && attempt == 0 {
                            // The daemon accepted the mutation, but its response was lost.
                            return;
                        }
                        json!({"jobs":[{"job_id":"job_01M45R71NDA5B7C9E1F3G5H7J9","state":"awaiting_approval","approval_id":"ap_01M45R71NE2K4M6P8R0T2V4X6Z"}],"dry_run":false,"replayed":false})
                    }
                    "jobs.get" => {
                        let d = zeus_verify::hash::hash_bytes(CONTENT);
                        json!({"job":{"id":"job_01M45R71NDA5B7C9E1F3G5H7J9","state":"completed","quarantined":false,
                            "spec":{"url_redacted":"https://portal.example.org/files/annual-report.pdf?\u{2026}","session_origin":"https://portal.example.org","source":{"kind":"agent","via":"jsonrpc","client_id":CRED_ID},"labels":{"ogun.run":"orun_x"}},
                            "probe":{"status":200,"final_url_redacted":"https://portal.example.org/files/annual-report.pdf","content_type":"application/pdf; charset=binary","validators":{"etag":"\"abc\"","last_modified":null}},
                            "result":{"path":file.as_ref().unwrap().to_str().unwrap(),"size_bytes":CONTENT.len(),"sha256":d.sha256.to_string(),"blake3":d.blake3.to_string(),"detected_mime":"application/pdf","completed_at":"2026-10-06T08:01:00.000Z"}}})
                    }
                    other => panic!("unexpected method {other}"),
                };
                let mut out =
                    serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result})).unwrap();
                out.push(b'\n');
                // An interleaved notification, which the client must skip.
                w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"events.event\",\"params\":{}}\n")
                    .await
                    .unwrap();
                w.write_all(&out).await.unwrap();
            }
        });
    }
}

#[tokio::test]
async fn submit_status_and_import_through_a_fake_zeusd() {
    let dir = tempfile::tempdir().unwrap();
    let (cfg, sock, root) = setup(dir.path());
    let listener = UnixListener::bind(&sock).unwrap();
    tokio::spawn(serve(listener, Mode::Normal));

    let bridge = ZeusBridge::connect(cfg).await.unwrap();
    assert_eq!(bridge.session().await.unwrap().kind, "service");
    let reg = bridge.register_import_root().await.unwrap();
    assert!(reg.path.ends_with("ogun"));

    let request = json!({"url":"https://portal.example.org/files/annual-report.pdf?sig=ZEUS-CANARY-abc","session_origin":"https://portal.example.org"});
    let opts = SubmitOptions {
        idempotency_key: Some("ni_x:1".into()),
        subdir: Some("orun_x".into()),
        ..SubmitOptions::default()
    };
    let submitted = bridge.submit_with(&request, &opts).await.unwrap();
    assert_eq!(
        submitted.approval_id.as_deref(),
        Some("ap_01M45R71NE2K4M6P8R0T2V4X6Z")
    );
    assert!(root.join("orun_x").is_dir());

    let status = bridge.status(&submitted.job_id).await.unwrap();
    let JobStatus::Succeeded(done) = status else {
        panic!("not succeeded")
    };
    assert_eq!(
        done.provenance.session_origin.as_deref(),
        Some("https://portal.example.org")
    );
    assert_eq!(
        done.provenance.declared_media_type.as_deref(),
        Some("application/pdf")
    );

    let store_root = dir.path().join("ogun-store");
    let store = zeus_artifact::ArtifactStore::open(
        &store_root,
        zeus_artifact::StoreConfig {
            product: "ogun".into(),
            version: "0.1.0".into(),
            min_free_bytes: 0,
            write_roots: vec![],
            ..zeus_artifact::StoreConfig::default()
        },
        Arc::new(zeus_fs::RealFs),
        Arc::new(zeus_control::SystemClock::new()),
        None,
    )
    .unwrap();
    let imported = bridge.import(&submitted.job_id, &store).await.unwrap();
    assert_eq!(imported.via, "job_view");
    assert_eq!(
        imported.blake3,
        zeus_verify::hash::hash_bytes(CONTENT).blake3.to_string()
    );
    assert_eq!(std::fs::read(&imported.object_path).unwrap(), CONTENT);
    let record = store.get(imported.artifact_id.parse().unwrap()).unwrap();
    assert_eq!(record.producer.product, "zeus");
    let text = serde_json::to_string(&record).unwrap();
    assert!(!text.contains("ZEUS-CANARY-"));
    assert!(text.contains("portal.example.org"));
    // Importing again returns the same record (ingest key `zeus:<job>`).
    let again = bridge.import(&submitted.job_id, &store).await.unwrap();
    assert_eq!(again.artifact_id, imported.artifact_id);
}

#[tokio::test]
async fn a_human_credential_or_a_wrong_server_proof_is_refused() {
    for (mode, want) in [
        (Mode::HumanCredential, "zeus.wrong_kind"),
        (Mode::WrongProof, "zeus.server_unverified"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, sock, _) = setup(dir.path());
        let listener = UnixListener::bind(&sock).unwrap();
        tokio::spawn(serve(listener, mode));
        let err: BridgeError = ZeusBridge::connect(cfg).await.unwrap_err();
        assert_eq!(err.code(), want);
    }
}

#[tokio::test]
async fn a_lost_enqueue_response_is_replayed_only_with_an_idempotency_key() {
    for keyed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, sock, _) = setup(dir.path());
        let listener = UnixListener::bind(&sock).unwrap();
        let enqueues = Arc::new(AtomicUsize::new(0));
        tokio::spawn(serve_with_count(
            listener,
            Mode::DropEnqueueResponse,
            enqueues.clone(),
        ));
        let bridge = ZeusBridge::connect(cfg).await.unwrap();
        bridge.register_import_root().await.unwrap();
        let request = json!({
            "url": "https://portal.example.org/report.pdf",
            "session_origin": "https://portal.example.org"
        });
        let opts = SubmitOptions {
            idempotency_key: keyed.then(|| "ni_x:1".to_owned()),
            ..SubmitOptions::default()
        };
        let submitted = bridge.submit_with(&request, &opts).await;
        if keyed {
            submitted.unwrap();
            assert_eq!(enqueues.load(Ordering::SeqCst), 2);
        } else {
            assert!(matches!(submitted, Err(BridgeError::Unavailable(_))));
            assert_eq!(enqueues.load(Ordering::SeqCst), 1);
            // A later explicit request reconnects successfully, without silently
            // retrying the earlier unkeyed mutation.
            bridge.submit_with(&request, &opts).await.unwrap();
            assert_eq!(enqueues.load(Ordering::SeqCst), 2);
        }
    }
}
