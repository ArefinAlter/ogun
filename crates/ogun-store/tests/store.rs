//! Run store behaviour without crashes.

use ogun_ir::{Catalogue, check};
use ogun_run::{Actor, Cause, NodeState, RunState};
use ogun_store::{Store, StoreError};
use ogun_types::IdGenerator;
use zeus_types::Timestamp;

const WF: &str = r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[
  {"id":"a","op":"acquire@1"},{"id":"b","op":"hash@1","inputs":["a.artifacts"]}]}"#;

fn setup() -> (tempfile::TempDir, Store, ogun_store::RunRecord) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("s.redb")).unwrap();
    let report = check(WF, &Catalogue::builtin());
    let run = store
        .create_run(
            report.workflow().unwrap(),
            &[],
            &IdGenerator::new(),
            Timestamp::now(),
        )
        .unwrap();
    (dir, store, run)
}

#[test]
fn unpartitioned_run_has_one_instance_per_node() {
    let (_d, store, run) = setup();
    let instances = store.instances(run.id).unwrap();
    assert_eq!(instances.len(), 2);
    assert!(
        instances
            .iter()
            .all(|i| i.state == NodeState::Pending && i.partition.is_empty())
    );
    assert_eq!(store.run_state(run.id).unwrap(), RunState::Pending);
    assert!(store.workflow_bytes(&run.workflow_hash).unwrap().is_some());
}

#[test]
fn invalid_transitions_write_nothing() {
    let (_d, store, run) = setup();
    let a = store.instances(run.id).unwrap()[0].id;
    let before = store.events_after(0, 100).unwrap().len();
    let err = store
        .transition(
            a,
            Cause::OutputsCommitted,
            Actor::Runtime,
            Timestamp::now(),
            None,
        )
        .unwrap_err();
    assert_eq!(err.code(), "run.invalid_transition");
    assert_eq!(store.events_after(0, 100).unwrap().len(), before);
}

#[test]
fn only_a_human_can_approve() {
    let (_d, store, run) = setup();
    let a = store.instances(run.id).unwrap()[0].id;
    store
        .transition(
            a,
            Cause::InputsAvailable,
            Actor::Runtime,
            Timestamp::now(),
            None,
        )
        .unwrap();
    store
        .transition(
            a,
            Cause::ApprovalRequired,
            Actor::Runtime,
            Timestamp::now(),
            None,
        )
        .unwrap();
    assert!(matches!(
        store.transition(
            a,
            Cause::ApprovalGranted,
            Actor::Runtime,
            Timestamp::now(),
            None
        ),
        Err(StoreError::InvalidTransition(_))
    ));
    let ni = store
        .transition(
            a,
            Cause::ApprovalGranted,
            Actor::Human,
            Timestamp::now(),
            None,
        )
        .unwrap();
    assert_eq!(ni.state, NodeState::Ready);
}

#[test]
fn a_denied_approval_does_not_mark_the_partition_done() {
    let (_d, store, run) = setup();
    let ids: Vec<_> = store
        .instances(run.id)
        .unwrap()
        .iter()
        .map(|i| i.id)
        .collect();
    let now = Timestamp::now();
    store
        .transition(ids[0], Cause::InputsAvailable, Actor::Runtime, now, None)
        .unwrap();
    store
        .transition(ids[0], Cause::ApprovalRequired, Actor::Runtime, now, None)
        .unwrap();
    store
        .transition(ids[0], Cause::ApprovalDenied, Actor::Human, now, None)
        .unwrap();
    store
        .transition(ids[1], Cause::NotNeeded, Actor::Runtime, now, None)
        .unwrap();
    assert_eq!(store.run_state(run.id).unwrap(), RunState::Succeeded);
    assert!(
        store
            .done_partitions(&run.workflow_hash)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn oversized_checkpoints_are_refused() {
    let (_d, store, run) = setup();
    let a = store.instances(run.id).unwrap()[0].id;
    let big = serde_json::json!({ "pad": "x".repeat(70_000) });
    assert!(matches!(
        store.transition(
            a,
            Cause::InputsAvailable,
            Actor::Runtime,
            Timestamp::now(),
            Some(big)
        ),
        Err(StoreError::CheckpointTooLarge)
    ));
}

#[test]
fn a_second_process_cannot_open_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.redb");
    let _first = Store::open(&path).unwrap();
    assert!(matches!(Store::open(&path), Err(StoreError::AlreadyOpen)));
}

#[test]
fn session_outputs_and_both_transitions_survive_restart_together() {
    use ogun_store::{Collection, DocWrite};
    use serde_json::json;

    let (dir, store, run) = setup();
    let a = store.instances(run.id).unwrap()[0].id;
    let now = Timestamp::now();
    for cause in [
        Cause::InputsAvailable,
        Cause::Dispatched,
        Cause::SessionRequired,
    ] {
        store
            .transition(a, cause, Actor::Runtime, now, None)
            .unwrap();
    }
    let outputs = json!([{"artifact_id":"imported-zeus-object"}]);
    let writes = [DocWrite::put(Collection::Outputs, a.to_string(), &outputs).unwrap()];
    let before = store.last_event_seq().unwrap();
    let done = store.complete_session(a, now, &writes).unwrap();
    assert_eq!(done.state, NodeState::Succeeded);
    let events = store.events_after(before, 10).unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].kind,
        ogun_store::EventKind::Transition {
            cause: Cause::SessionAvailable,
            ..
        }
    ));
    assert!(matches!(
        events[1].kind,
        ogun_store::EventKind::Transition {
            cause: Cause::OutputsCommitted,
            ..
        }
    ));
    drop(store);

    let store = Store::open(&dir.path().join("s.redb")).unwrap();
    assert!(store.recover(Timestamp::now()).unwrap().is_empty());
    assert_eq!(store.instance(a).unwrap().state, NodeState::Succeeded);
    assert_eq!(
        store
            .get_doc::<serde_json::Value>(Collection::Outputs, &a.to_string())
            .unwrap(),
        Some(outputs.clone())
    );
    // A raced cancellation or repeated completion cannot replace outputs or add events.
    let before = store.last_event_seq().unwrap();
    let replacement = [DocWrite::put(Collection::Outputs, a.to_string(), &json!([])).unwrap()];
    assert!(store.complete_session(a, now, &replacement).is_err());
    assert_eq!(store.last_event_seq().unwrap(), before);
    assert_eq!(
        store
            .get_doc::<serde_json::Value>(Collection::Outputs, &a.to_string())
            .unwrap(),
        Some(outputs)
    );
}
