//! Durability: a process killed in the middle of a backfill loses nothing it committed, and on
//! reopening, recovery plus a fresh backfill plan runs only the unfinished partitions. This is the
//! shape of the phase O1 exit test (ROADMAP), without real downloads.

use std::path::Path;
use std::process::Command;

use ogun_ir::{Catalogue, check};
use ogun_run::{Actor, Cause, NodeState, RunState};
use ogun_store::Store;
use ogun_types::IdGenerator;
use zeus_types::Timestamp;

const WORKFLOW: &str = r#"{
  "ir_version": 0, "workflow": "noaa-daily", "version": 1,
  "partitions": { "kind": "time", "granularity": "day", "start": "2026-01-01" },
  "nodes": [
    { "id": "fetch", "op": "acquire@1" },
    { "id": "hash", "op": "hash@1", "inputs": ["fetch.artifacts"] }
  ]
}"#;

const PARTITIONS: usize = 10;
const FINISHED_BEFORE_CRASH: usize = 4;

fn keys() -> Vec<String> {
    (1..=PARTITIONS)
        .map(|d| format!("2026-01-{d:02}"))
        .collect()
}

fn now() -> Timestamp {
    Timestamp::now()
}

/// Drives one partition's two nodes to `succeeded`.
fn finish_partition(store: &Store, instances: &[ogun_store::NodeInstance], partition: &str) {
    for ni in instances.iter().filter(|i| i.partition == partition) {
        for cause in [
            Cause::InputsAvailable,
            Cause::Dispatched,
            Cause::OutputsCommitted,
        ] {
            store
                .transition(ni.id, cause, Actor::Runtime, now(), None)
                .unwrap();
        }
    }
}

/// Runs in the child process: finishes some partitions, leaves one node running with a
/// checkpoint, then aborts without any shutdown.
fn child(path: &Path) -> ! {
    let report = check(WORKFLOW, &Catalogue::builtin());
    let wf = report.workflow().unwrap();
    let store = Store::open(path).unwrap();
    let run = store
        .create_run(wf, &keys(), &IdGenerator::new(), now())
        .unwrap();
    std::fs::write(path.with_extension("run"), run.id.to_string()).unwrap();
    let instances = store.instances(run.id).unwrap();
    for key in keys().iter().take(FINISHED_BEFORE_CRASH) {
        finish_partition(&store, &instances, key);
    }
    let next = instances
        .iter()
        .find(|i| i.partition == keys()[FINISHED_BEFORE_CRASH])
        .unwrap();
    store
        .transition(next.id, Cause::InputsAvailable, Actor::Runtime, now(), None)
        .unwrap();
    store
        .transition(
            next.id,
            Cause::Dispatched,
            Actor::Runtime,
            now(),
            Some(serde_json::json!({ "offset": 4194304 })),
        )
        .unwrap();
    std::process::abort();
}

#[test]
fn killed_backfill_resumes_only_unfinished_partitions() {
    if let Ok(path) = std::env::var("OGUN_STORE_CRASH_CHILD") {
        child(Path::new(&path));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runs.redb");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "killed_backfill_resumes_only_unfinished_partitions",
            "--nocapture",
        ])
        .env("OGUN_STORE_CRASH_CHILD", &path)
        .status()
        .unwrap();
    assert!(!status.success(), "the child must die by abort");

    let store = Store::open(&path).unwrap();
    let run_id = std::fs::read_to_string(path.with_extension("run"))
        .unwrap()
        .parse()
        .unwrap();
    let run = store.run(run_id).unwrap();

    // Everything committed before the abort is there.
    let done = store.done_partitions(&run.workflow_hash).unwrap();
    assert_eq!(done.len(), FINISHED_BEFORE_CRASH);
    let before = store.instances(run.id).unwrap();
    let running: Vec<_> = before
        .iter()
        .filter(|i| i.state == NodeState::Running)
        .collect();
    assert_eq!(running.len(), 1);
    assert_eq!(store.run_state(run.id).unwrap(), RunState::Running);

    // Recovery returns the interrupted node to ready and keeps its checkpoint.
    let recovered = store.recover(now()).unwrap();
    assert_eq!(recovered, vec![running[0].id]);
    let after = store.instances(run.id).unwrap();
    let resumed = after.iter().find(|i| i.id == running[0].id).unwrap();
    assert_eq!(resumed.state, NodeState::Ready);
    assert_eq!(
        resumed.checkpoint,
        Some(serde_json::json!({ "offset": 4194304 }))
    );
    assert_eq!(resumed.attempt, 1);

    // A new backfill plan over the same range runs only what was not finished.
    let plan = ogun_sched::plan(&keys(), &done);
    assert_eq!(plan.up_to_date, keys()[..FINISHED_BEFORE_CRASH]);
    assert_eq!(plan.to_run, keys()[FINISHED_BEFORE_CRASH..]);

    // Events have no gaps and end with the recovery.
    let events = store.events_after(0, usize::MAX).unwrap();
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.seq, i as u64 + 1);
    }
    assert!(matches!(
        events.last().unwrap().kind,
        ogun_store::EventKind::Transition {
            cause: Cause::Recovered,
            ..
        }
    ));
}
