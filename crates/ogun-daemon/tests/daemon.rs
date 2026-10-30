//! Drives a real `ogund` on a temporary state directory through the O-15 client.

use std::time::Duration;

use ogun_api::auth::ClientKind;
use ogun_api::methods::{self as m, PartitionSelector, name};
use ogun_api::{Client, OgunPaths};
use ogun_types::{IdGenerator, Timestamp};
use serde_json::{Value, json};

fn info() -> m::ClientInfo {
    m::ClientInfo {
        name: "test".into(),
        version: "0".into(),
    }
}

async fn setup(dir: &std::path::Path) -> (OgunPaths, ogun_daemon::Running) {
    let paths = OgunPaths::rooted(dir, None);
    // A service credential for the push test, created the way the CLI would (before the daemon).
    let ids = IdGenerator::new();
    let (mut creds, _) = ogun_daemon::creds::Credentials::load_or_init(&paths, &ids).unwrap();
    creds
        .add(&paths, &ids, "ingest", ClientKind::Service)
        .unwrap();
    creds.add(&paths, &ids, "agent", ClientKind::Agent).unwrap();
    drop(creds);
    let running = ogun_daemon::start(paths.clone()).await.unwrap();
    (paths, running)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_run_and_policy() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, running) = setup(dir.path()).await;
    let mut cli = Client::connect(&paths, "cli", info()).await.unwrap();
    assert_eq!(cli.kind(), ClientKind::Human);

    let mut doc: Value =
        serde_json::from_str(include_str!("../../../examples/noaa-daily.workflow.json")).unwrap();
    // Keep partition/lifecycle coverage local now that acquire executes real requests.
    doc["nodes"] = json!([{ "id": "manifest", "op": "manifest@1" }]);
    let put: m::PutWorkflowResult = cli
        .call(
            name::WORKFLOWS_PUT,
            &m::PutWorkflowParams {
                workflow: doc.clone(),
                idempotency_key: "k1".into(),
                code: Default::default(),
            },
        )
        .await
        .unwrap();
    assert!(put.created && put.approvals.is_empty());
    // The same key and parameters return the first result.
    let again: m::PutWorkflowResult = cli
        .call(
            name::WORKFLOWS_PUT,
            &m::PutWorkflowParams {
                workflow: doc,
                idempotency_key: "k1".into(),
                code: Default::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(again, put);

    let run: m::RunView = cli
        .call(
            name::RUNS_START,
            &m::StartRunParams {
                workflow: "noaa-daily".into(),
                version: None,
                workflow_hash: None,
                partitions: PartitionSelector::Keys {
                    keys: vec!["2024-01-02".into()],
                },
                idempotency_key: "r1".into(),
            },
        )
        .await
        .unwrap();
    // The native manifest operator runs through the daemon and commits its output.
    let mut state = run.state;
    for _ in 0..50 {
        let v: m::RunView = cli
            .call(name::RUNS_GET, &m::RunRef { run_id: run.run_id })
            .await
            .unwrap();
        state = v.state;
        if matches!(
            state,
            ogun_run::RunState::Failed | ogun_run::RunState::Succeeded
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(state, ogun_run::RunState::Succeeded);
    let status: m::PartitionStatusResult = cli
        .call(
            name::PARTITIONS_STATUS,
            &m::PartitionStatusParams {
                workflow: "noaa-daily".into(),
                version: None,
                from: Some("2024-01-01".into()),
                to: Some("2024-01-03".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(status.ranges.iter().map(|r| r.count).sum::<u64>(), 3);

    // Agents never decide, whatever they send.
    let mut agent = Client::connect(&paths, "agent", info()).await.unwrap();
    let denied = agent
        .call_value(name::APPROVALS_DECIDE, json!({"approval_id": "oap_01M5M59YMG90Q3CTM7DQKXBJ66", "decision": "approve", "request_hash": "x", "evidence": {"channel": "terminal"}}))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), "api.permission_denied");
    let denied = agent
        .call_value(name::SECRETS_LIST, json!({}))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), "api.permission_denied");
    // Agent credentials cannot bypass the read-only MCP boundary through the daemon API.
    for method in [name::WORKFLOWS_PUT, name::RUNS_START, name::RUNS_CANCEL] {
        let denied = agent.call_value(method, json!({})).await.unwrap_err();
        assert_eq!(denied.code(), "api.permission_denied", "{method}");
    }
    // A human connection without a terminal cannot change trust.
    let denied = cli
        .call_value(name::CREDENTIALS_ADD, json!({"name": "x", "kind": "agent"}))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), "api.permission_denied");
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_push_seals_a_window_into_a_run() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, running) = setup(dir.path()).await;
    let mut cli = Client::connect(&paths, "cli", info()).await.unwrap();
    let doc: Value = serde_json::from_str(include_str!(
        "../../../examples/air-quality-stream.workflow.json"
    ))
    .unwrap();
    let _: m::PutWorkflowResult = cli
        .call(
            name::WORKFLOWS_PUT,
            &m::PutWorkflowParams {
                workflow: doc,
                idempotency_key: "s1".into(),
                code: Default::default(),
            },
        )
        .await
        .unwrap();
    let mut ingest = Client::connect(&paths, "ingest", info()).await.unwrap();
    let now = Timestamp::now().unix_millis();
    let old = now - 10 * 60_000;
    let rec = |ts: i64, seq: u64| m::StreamRecord {
        event_time_ms: ts,
        key: "dev-1".into(),
        payload: json!({"device_id": "dev-1", "seq": seq, "ts_ms": ts, "pm25_ugm3_x10": 123}),
    };
    let p = m::PushParams {
        workflow: "air-quality-stream".into(),
        node: "sensors".into(),
        first_seq: 0,
        records: vec![rec(old, 0), rec(old + 1_000, 1), rec(now - 60_000, 2)],
    };
    let mut overflow = p.clone();
    overflow.first_seq = u64::MAX;
    let error = ingest
        .call::<_, m::PushResult>(name::STREAMS_PUSH, &overflow)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "api.invalid_params");
    let r: m::PushResult = ingest.call(name::STREAMS_PUSH, &p).await.unwrap();
    assert_eq!(r.accepted_through, 2);
    // A retry of the same batch is not ingested twice.
    let r: m::PushResult = ingest.call(name::STREAMS_PUSH, &p).await.unwrap();
    assert_eq!(r.accepted_through, 2);
    let mut found = None;
    for _ in 0..40 {
        let runs: m::ListRunsResult = cli
            .call(
                name::RUNS_LIST,
                &m::ListRunsParams {
                    workflow: Some("air-quality-stream".into()),
                    before: None,
                    limit: Some(10),
                },
            )
            .await
            .unwrap();
        if let Some(r) = runs.runs.first() {
            found = Some(r.run_id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let run_id = found.expect("a sealed window starts a run");
    let arts: m::ListArtifactsResult = cli
        .call(name::ARTIFACTS_LIST, &m::ListArtifactsParams { run_id })
        .await
        .unwrap();
    let sealed = arts
        .artifacts
        .iter()
        .find(|a| a.output == "window")
        .expect("the sealed window is the source output");
    let got: m::GetArtifactResult = cli
        .call(
            name::ARTIFACTS_GET,
            &m::GetArtifactParams {
                blake3: sealed.blake3.clone(),
                inline_max_bytes: 1 << 20,
            },
        )
        .await
        .unwrap();
    assert!(got.content_base64.is_some());
    let streams: m::ListStreamsResult = cli.call(name::STREAMS_LIST, &json!({})).await.unwrap();
    assert_eq!(streams.streams.len(), 1);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_enrolled_edge_syncs_a_sealed_window_to_the_hub() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let (paths, running) = setup(dir.path()).await;
    let key = ogun_sync::keys::EdgeKey::from_seed(&[7u8; 32]);
    let edge_id = IdGenerator::new().edge_id();
    // Enrolment needs a person on the host terminal; the test writes the row the way it would.
    let view = m::EdgeView {
        edge_id,
        name: "site-a".into(),
        public_key: key.public_key(),
        enrolled_at: Timestamp::now(),
        last_sync_at: None,
    };
    running
        .daemon
        .store
        .put_doc(ogun_store::Collection::Edges, &edge_id.to_string(), &view)
        .unwrap();

    let blob = b"{\"key\":\"dev-1\",\"event_ms\":1}\n".repeat(40_000);
    let digest = blake3::hash(&blob).to_hex().to_string();
    let offer = ogun_edge::Offer {
        edge_id: edge_id.to_string(),
        items: vec![ogun_edge::Item {
            asset: format!("{edge_id}/air-quality-stream/sensors"),
            partition: "w20261023T081500Z".into(),
            revision: 1,
            blake3: digest.clone(),
            size_bytes: blob.len() as u64,
        }],
    };
    let (offer_json, signature) = ogun_sync::signing::sign_offer(&key, &offer).unwrap();
    let mut svc = Client::connect(&paths, "ingest", info()).await.unwrap();
    let forged = svc
        .call_value(
            name::SYNC_OFFER,
            json!({"edge_id": edge_id, "offer": offer_json, "signature": ogun_sync::keys::EdgeKey::from_seed(&[8u8; 32]).sign(b"x")}),
        )
        .await
        .unwrap_err();
    assert_eq!(forged.code(), "sync.bad_signature");
    let want: m::SyncOfferResult = svc
        .call(
            name::SYNC_OFFER,
            &m::SyncOfferParams {
                edge_id,
                offer: offer_json,
                signature,
            },
        )
        .await
        .unwrap();
    assert!(want.want.to_string().contains(&digest));
    let chunk = usize::try_from(ogun_sync::CHUNK_BYTES).unwrap();
    let parts: Vec<&[u8]> = blob.chunks(chunk).collect();
    let mut complete = false;
    for (i, part) in parts.iter().enumerate() {
        let r: m::SyncChunkResult = svc
            .call(
                name::SYNC_CHUNK,
                &m::SyncChunkParams {
                    edge_id,
                    partition: "w20261023T081500Z".into(),
                    blake3: digest.clone(),
                    index: u32::try_from(i).unwrap(),
                    total: u32::try_from(parts.len()).unwrap(),
                    data_base64: base64::engine::general_purpose::STANDARD.encode(part),
                },
            )
            .await
            .unwrap();
        complete = r.complete;
    }
    assert!(complete, "the last chunk completes the object");
    let edges: m::ListEdgesResult = svc.call(name::EDGES_LIST, &json!({})).await.unwrap();
    assert!(edges.edges[0].last_sync_at.is_some());
    running.stop().await;
}

/// The peer check on every platform: the daemon learns which executable is on the other end of the connection,
/// and only a pinned Studio binary reaches the Studio-callable methods and `studio` approval evidence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn studio_is_recognised_by_its_executable() {
    let dir = tempfile::tempdir().unwrap();
    let me = std::env::current_exe().unwrap();
    let paths = OgunPaths::rooted(dir.path(), None);
    std::fs::create_dir_all(&paths.state_dir).unwrap();
    let ids = IdGenerator::new();
    let (mut creds, _) = ogun_daemon::creds::Credentials::load_or_init(&paths, &ids).unwrap();
    creds
        .add(&paths, &ids, "studio", ClientKind::Human)
        .unwrap();
    creds.add(&paths, &ids, "agent", ClientKind::Agent).unwrap();
    drop(creds);

    // Not pinned: a human credential alone is not a trust surface.
    let running = ogun_daemon::start(paths.clone()).await.unwrap();
    let mut c = Client::connect(&paths, "studio", info()).await.unwrap();
    assert_eq!(c.surface(), ogun_api::policy::Surface::None);
    assert!(!c.interactive());
    let refused = c
        .call_value(name::SECRETS_PUT, json!({"name": "k", "value": "v"}))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "api.permission_denied");
    drop(c);
    running.stop().await;

    // Pinned as Studio: the peer's executable is read from the operating system, not from the client.
    std::fs::write(
        paths.config_file(),
        serde_json::to_vec(&json!({ "trusted_studio_binaries": [me.display().to_string()] }))
            .unwrap(),
    )
    .unwrap();
    let running = ogun_daemon::start(paths.clone()).await.unwrap();
    let mut c = Client::connect(&paths, "studio", info()).await.unwrap();
    assert_eq!(c.surface(), ogun_api::policy::Surface::Studio);
    assert!(c.interactive());
    assert!(c.methods().iter().any(|m| m == name::SECRETS_PUT));
    assert!(!c.methods().iter().any(|m| m == name::CREDENTIALS_ADD));
    let ok = c
        .call_value(name::SECRETS_PUT, json!({"name": "k", "value": "v"}))
        .await
        .unwrap();
    assert_eq!(ok, json!({}));
    let names = c.call_value(name::SECRETS_LIST, json!({})).await.unwrap();
    assert_eq!(names, json!({"names": ["k"]}));
    // Trust changes stay on a terminal.
    let denied = c
        .call_value(name::CREDENTIALS_ADD, json!({"name": "x", "kind": "agent"}))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), "api.permission_denied");
    // An agent connecting from the same pinned executable is still an agent.
    let mut agent = Client::connect(&paths, "agent", info()).await.unwrap();
    assert_eq!(agent.surface(), ogun_api::policy::Surface::None);
    let denied = agent
        .call_value(name::SECRETS_PUT, json!({"name": "k2", "value": "v"}))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), "api.permission_denied");
    running.stop().await;
}
