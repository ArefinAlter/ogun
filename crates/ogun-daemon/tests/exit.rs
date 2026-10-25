//! The O1 exit tests (ROADMAP): `ogund` killed with SIGKILL in the middle of work, restarted, and the
//! work resumes exactly where it stopped.
//!
//! * Batch: a partitioned backfill over 121 days. Partitions finished before the kill are not run again,
//!   every other one finishes after the restart, and a second backfill over the range has nothing to do.
//! * Stream twin: records pushed into an open window survive the kill (the daemon's ingest log is
//!   replayed at restart), a retried push is not ingested twice, and the window then seals with exactly
//!   those records.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ogun_api::methods::{self as m, PartitionSelector, name};
use ogun_api::{Client, OgunPaths};
use ogun_run::{NodeState, RunState};
use ogun_types::{IdGenerator, Timestamp};
use serde_json::{Value, json};

fn info() -> m::ClientInfo {
    m::ClientInfo {
        name: "exit-test".into(),
        version: "0".into(),
    }
}

fn spawn(home: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_ogund"))
        .arg("--home")
        .arg(home)
        .env("OGUN_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ogund starts")
}

async fn connect(paths: &OgunPaths, credential: &str) -> Client {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match Client::connect(paths, credential, info()).await {
            Ok(c) => return c,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!("ogund did not answer: {e}"),
        }
    }
}

fn kill(mut child: Child) {
    // `Child::kill` is SIGKILL on Unix: no shutdown path runs.
    child.kill().expect("kill -9");
    let _ = child.wait();
}

fn backfill_workflow() -> Value {
    json!({
        "ir_version": 0,
        "workflow": "exit-backfill",
        "version": 1,
        "partitions": { "kind": "time", "granularity": "day", "start": "2024-01-01", "end": "2024-04-30" },
        "nodes": [
            { "id": "m", "op": "manifest@1" },
            { "id": "h", "op": "hash@1", "inputs": ["m.manifest"] }
        ]
    })
}

async fn instances(c: &mut Client, run_id: ogun_types::RunId) -> m::RunView {
    c.call(name::RUNS_GET, &m::RunRef { run_id })
        .await
        .expect("runs.get")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_backfill_resumes_only_unfinished_partitions() {
    let dir = tempfile::tempdir().unwrap();
    let paths = OgunPaths::rooted(dir.path(), None);
    let child = spawn(dir.path());
    let mut c = connect(&paths, "cli").await;
    let _: m::PutWorkflowResult = c
        .call(
            name::WORKFLOWS_PUT,
            &m::PutWorkflowParams {
                workflow: backfill_workflow(),
                idempotency_key: "p".into(),
                code: Default::default(),
            },
        )
        .await
        .unwrap();
    let run: m::RunView = c
        .call(
            name::RUNS_START,
            &m::StartRunParams {
                workflow: "exit-backfill".into(),
                version: None,
                workflow_hash: None,
                partitions: PartitionSelector::Range {
                    from: "2024-01-01".into(),
                    to: "2024-04-30".into(),
                    force: false,
                },
                idempotency_key: "b".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(run.partition_count, 121);

    // Let part of the backfill finish, then kill -9.
    let deadline = Instant::now() + Duration::from_secs(60);
    let before = loop {
        let v = instances(&mut c, run.run_id).await;
        let done = v
            .nodes
            .iter()
            .filter(|n| n.state == NodeState::Succeeded)
            .count();
        if done >= 40 {
            break v;
        }
        assert!(Instant::now() < deadline, "the backfill made no progress");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    drop(c);
    kill(child);
    let finished_before: BTreeMap<_, _> = before
        .nodes
        .iter()
        .filter(|n| n.state == NodeState::Succeeded)
        .map(|n| (n.instance_id, (n.attempt, n.updated_at)))
        .collect();
    assert!(finished_before.len() < 242, "the kill must land mid-run");

    // Restart and let it finish.
    let child = spawn(dir.path());
    let mut c = connect(&paths, "cli").await;
    let deadline = Instant::now() + Duration::from_secs(120);
    let after = loop {
        let v = instances(&mut c, run.run_id).await;
        if matches!(
            v.state,
            RunState::Succeeded | RunState::Failed | RunState::Cancelled
        ) {
            break v;
        }
        assert!(
            Instant::now() < deadline,
            "the run did not finish after the restart: {:?}",
            v.state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(after.state, RunState::Succeeded);
    assert_eq!(after.nodes.len(), 242);
    for n in &after.nodes {
        if let Some((attempt, at)) = finished_before.get(&n.instance_id) {
            assert_eq!(
                (n.attempt, n.updated_at),
                (*attempt, *at),
                "{} ran again",
                n.instance_id
            );
        }
    }
    // Only what was running at the kill ran twice.
    let repeated = after.nodes.iter().filter(|n| n.attempt > 1).count();
    assert!(repeated <= 4, "{repeated} instances ran more than once");

    // Every partition is now up to date for this workflow hash.
    let plan: m::PlanBackfillResult = c
        .call(
            name::BACKFILLS_PLAN,
            &m::PlanBackfillParams {
                workflow: "exit-backfill".into(),
                from: "2024-01-01".into(),
                to: "2024-04-30".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!((plan.to_run, plan.up_to_date), (0, 121));
    drop(c);
    kill(child);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_stream_keeps_its_open_window() {
    let dir = tempfile::tempdir().unwrap();
    let paths = OgunPaths::rooted(dir.path(), None);
    // The ingest bridge's credential, made the way `ogun credentials add` would, before the daemon starts.
    {
        let ids = IdGenerator::new();
        let (mut creds, _) = ogun_daemon::creds::Credentials::load_or_init(&paths, &ids).unwrap();
        creds
            .add(&paths, &ids, "ingest", ogun_api::auth::ClientKind::Service)
            .unwrap();
    }
    let child = spawn(dir.path());
    let mut cli = connect(&paths, "cli").await;
    let doc: Value = serde_json::from_str(include_str!(
        "../../../examples/air-quality-stream.workflow.json"
    ))
    .unwrap();
    let _: m::PutWorkflowResult = cli
        .call(
            name::WORKFLOWS_PUT,
            &m::PutWorkflowParams {
                workflow: doc,
                idempotency_key: "s".into(),
                code: Default::default(),
            },
        )
        .await
        .unwrap();
    let now = Timestamp::now().unix_millis();
    // Ten minutes ago, aligned to a window start, so both records fall in one window.
    let w = (now - 10 * 60_000) / 60_000 * 60_000;
    let rec = |ts: i64, seq: u64| m::StreamRecord {
        event_time_ms: ts,
        key: "dev-1".into(),
        payload: json!({"device_id": "dev-1", "seq": seq, "ts_ms": ts, "pm25_ugm3_x10": 120 + seq}),
    };
    let first = m::PushParams {
        workflow: "air-quality-stream".into(),
        node: "sensors".into(),
        first_seq: 0,
        records: vec![rec(w + 1_000, 0), rec(w + 2_000, 1)],
    };
    let mut ingest = connect(&paths, "ingest").await;
    let r: m::PushResult = ingest.call(name::STREAMS_PUSH, &first).await.unwrap();
    assert_eq!(r.accepted_through, 1);
    // The watermark (newest event minus 120 s) is still before the window's end: it stays open.
    drop(ingest);
    drop(cli);
    kill(child);

    let child = spawn(dir.path());
    let mut cli = connect(&paths, "cli").await;
    let mut ingest = connect(&paths, "ingest").await;
    // The bridge never saw an answer it trusted, so it retries the same batch, then sends a newer record.
    let r: m::PushResult = ingest.call(name::STREAMS_PUSH, &first).await.unwrap();
    assert_eq!(r.accepted_through, 1);
    let later = m::PushParams {
        workflow: "air-quality-stream".into(),
        node: "sensors".into(),
        first_seq: 2,
        records: vec![rec(now - 60_000, 2)],
    };
    let _: m::PushResult = ingest.call(name::STREAMS_PUSH, &later).await.unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let sealed = loop {
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
            let arts: m::ListArtifactsResult = cli
                .call(
                    name::ARTIFACTS_LIST,
                    &m::ListArtifactsParams { run_id: r.run_id },
                )
                .await
                .unwrap();
            if let Some(a) = arts.artifacts.into_iter().find(|a| a.output == "window") {
                break a;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the window did not seal after the restart"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(sealed.partition, ogun_stream::window_key(w));
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
    let bytes = std::fs::read(&got.path).unwrap();
    assert_eq!(blake3::hash(&bytes).to_hex().as_str(), sealed.blake3);
    let lines: Vec<&[u8]> = bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        2,
        "exactly the two records pushed before the kill, once each"
    );
    drop(cli);
    drop(ingest);
    kill(child);
}
