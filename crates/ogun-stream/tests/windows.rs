//! End-to-end window behaviour: sealing on watermark, slow and idle devices, late data producing
//! revisions, duplicates, the `late` output, crash re-seal idempotency and clock clamping.

use ogun_stream::{Action, Ingested, Record, WindowSpec, WindowState, Windower, seal, window_key};
use serde_json::json;

const MIN: i64 = 60_000;

fn spec() -> WindowSpec {
    WindowSpec {
        size_ms: 60_000,
        allowed_lateness_ms: 10 * 60_000,
        reseal_after_ms: 30_000,
        max_delay_ms: 30_000,
        idle_timeout_ms: 5 * 60_000,
        max_future_ms: 5 * 60_000,
    }
}

fn rec(key: &str, event_ms: i64, seq: u64) -> Record {
    Record {
        key: key.into(),
        event_ms,
        seq,
        dedupe: Some(format!("{key}/{seq}")),
        payload: json!({ "pm25": 41 }),
        clamped: false,
    }
}

/// Seals everything `poll` asks for, as the daemon would, and returns (start, revision, digest).
fn drive(w: &mut Windower, now: i64) -> Vec<(i64, u32, String, bool)> {
    let mut out = Vec::new();
    for action in w.poll(now) {
        if let Action::Seal {
            window_start,
            revision,
            records,
        } = action
        {
            let batch = seal(&records).unwrap();
            let new = w.seal_committed(window_start, &batch.blake3);
            out.push((window_start, revision, batch.blake3, new));
        }
    }
    out
}

#[test]
fn a_window_seals_when_the_watermark_passes_its_end() {
    let mut w = Windower::new(spec()).unwrap();
    let t0 = 100 * MIN;
    assert_eq!(
        w.ingest(rec("d1", t0 + 1_000, 1), t0 + 1_000),
        Ingested::Accepted { window_start: t0 }
    );
    assert!(drive(&mut w, t0 + 50_000).is_empty());
    // A record 30 s past the end moves d1's watermark to the end.
    w.ingest(rec("d1", t0 + MIN + 30_000, 2), t0 + MIN + 30_000);
    let sealed = drive(&mut w, t0 + MIN + 30_000);
    assert_eq!(sealed.len(), 1);
    assert_eq!((sealed[0].0, sealed[0].1), (t0, 1));
    assert_eq!(w.state(t0), Some(WindowState::Sealed));
    assert_eq!(window_key(t0), "w19700101T014000Z");
}

#[test]
fn a_slow_device_holds_the_watermark_until_it_goes_idle() {
    let mut w = Windower::new(spec()).unwrap();
    let t0 = 100 * MIN;
    w.ingest(rec("fast", t0 + 1_000, 1), t0 + 1_000);
    w.ingest(rec("slow", t0 + 2_000, 2), t0 + 2_000);
    w.ingest(rec("fast", t0 + 3 * MIN, 3), t0 + 3 * MIN);
    // "slow" is behind and still active: no seal yet.
    assert!(drive(&mut w, t0 + 3 * MIN).is_empty());
    // After the idle timeout it no longer holds the watermark back.
    let sealed = drive(&mut w, t0 + 2_000 + 5 * MIN);
    assert_eq!(sealed.len(), 1);
}

#[test]
fn late_records_make_a_new_revision_and_duplicates_do_not() {
    let mut w = Windower::new(spec()).unwrap();
    let t0 = 100 * MIN;
    w.ingest(rec("d1", t0 + 1_000, 1), t0 + 1_000);
    w.ingest(rec("d1", t0 + 2 * MIN, 2), t0 + 2 * MIN);
    let first = drive(&mut w, t0 + 2 * MIN);
    assert_eq!(first[0].1, 1);

    // A duplicate of record 1 arrives late: dropped, nothing to re-seal.
    assert_eq!(
        w.ingest(rec("d1", t0 + 1_000, 1), t0 + 3 * MIN),
        Ingested::Duplicate
    );
    // A genuinely new late record within the lateness revises the window.
    assert_eq!(
        w.ingest(rec("d2", t0 + 5_000, 9), t0 + 3 * MIN),
        Ingested::Revises { window_start: t0 }
    );
    assert_eq!(w.state(t0), Some(WindowState::Dirty));
    assert!(
        drive(&mut w, t0 + 3 * MIN + 10_000).is_empty(),
        "debounce holds the re-seal"
    );
    let second = drive(&mut w, t0 + 3 * MIN + 30_000);
    let revs: Vec<_> = second.iter().filter(|s| s.0 == t0).collect();
    assert_eq!(revs.len(), 1);
    assert_eq!(revs[0].1, 2);
    assert!(revs[0].3);
    assert_ne!(revs[0].2, first[0].2);
    assert_eq!(w.counters().revisions, 1);
}

#[test]
fn after_the_lateness_the_window_is_final_and_records_go_to_late() {
    let mut w = Windower::new(spec()).unwrap();
    let t0 = 100 * MIN;
    w.ingest(rec("d1", t0 + 1_000, 1), t0 + 1_000);
    let now = t0 + MIN + 10 * MIN;
    drive(&mut w, now);
    let finals: Vec<_> = w
        .poll(now)
        .into_iter()
        .filter(|a| matches!(a, Action::Finalized { .. }))
        .collect();
    assert_eq!(finals.len(), 1);
    assert_eq!(w.state(t0), Some(WindowState::Final));
    assert_eq!(w.ingest(rec("d1", t0 + 2_000, 2), now + 1), Ingested::Late);
    // A record for an old window that never existed also goes to late.
    assert_eq!(w.ingest(rec("d1", 5 * MIN, 3), now + 1), Ingested::Late);
    assert_eq!(w.take_late().len(), 2);
    assert_eq!(w.counters().late, 2);
}

#[test]
fn re_sealing_the_same_records_is_the_same_revision() {
    let mut w = Windower::new(spec()).unwrap();
    let t0 = 100 * MIN;
    w.ingest(rec("d1", t0 + 1_000, 1), t0 + 1_000);
    w.ingest(rec("d1", t0 + 2 * MIN, 2), t0 + 2 * MIN);
    let actions = w.poll(t0 + 2 * MIN);
    let Action::Seal {
        window_start,
        records,
        ..
    } = &actions[0]
    else {
        panic!()
    };
    let digest = seal(records).unwrap().blake3;
    // The checkpoint taken before the commit is what a crashed daemon restarts from.
    let checkpoint = serde_json::to_string(&w).unwrap();
    assert!(w.seal_committed(*window_start, &digest));

    let mut restarted: Windower = serde_json::from_str(&checkpoint).unwrap();
    assert_eq!(restarted.state(t0), Some(WindowState::Sealing));
    // The restarted daemon re-materializes the same bytes.
    assert_eq!(seal(records).unwrap().blake3, digest);
    assert!(restarted.seal_committed(t0, &digest));
    assert_eq!(restarted.revision(t0), Some((1, Some(digest.as_str()))));
}

#[test]
fn future_event_times_are_clamped() {
    let mut w = Windower::new(spec()).unwrap();
    let now = 100 * MIN;
    assert_eq!(
        w.ingest(rec("cheap-clock", now + 3_600_000, 1), now),
        Ingested::Accepted { window_start: now }
    );
    assert_eq!(w.counters().clamped, 1);
}

#[test]
fn specs_outside_the_limits_are_refused() {
    let mut s = spec();
    s.size_ms = 7_000;
    assert!(Windower::new(s).is_err());
    s.size_ms = 500;
    assert!(Windower::new(s).is_err());
    s.size_ms = 60_000;
    s.allowed_lateness_ms = 8 * 86_400_000;
    assert!(Windower::new(s).is_err());
}

#[test]
fn records_arriving_during_a_seal_are_included_in_the_next_revision() {
    let mut w = Windower::new(spec()).unwrap();
    let t0 = 100 * MIN;
    w.ingest(rec("d1", t0 + 1_000, 1), t0 + 1_000);
    w.ingest(rec("d1", t0 + 2 * MIN, 2), t0 + 2 * MIN);
    let actions = w.poll(t0 + 2 * MIN);
    let Action::Seal { records, .. } = &actions[0] else {
        panic!()
    };
    let first = seal(records).unwrap();
    w.ingest(rec("d1", t0 + 5_000, 3), t0 + 2 * MIN + 1);
    assert!(w.seal_committed(t0, &first.blake3));
    assert_eq!(w.state(t0), Some(WindowState::Dirty));
    let checkpoint = serde_json::to_string(&w).unwrap();
    let mut restarted: Windower = serde_json::from_str(&checkpoint).unwrap();
    let actions = restarted.poll(t0 + 3 * MIN);
    let Action::Seal {
        records, revision, ..
    } = &actions[0]
    else {
        panic!()
    };
    assert_eq!(*revision, 2);
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|r| r.seq == 3));
}

#[test]
fn lateness_deadline_is_enforced_even_before_poll() {
    let mut w = Windower::new(spec()).unwrap();
    let t0 = 100 * MIN;
    w.ingest(rec("d1", t0 + 1_000, 1), t0 + 1_000);
    assert_eq!(
        w.ingest(rec("d1", t0 + 2_000, 2), t0 + 11 * MIN),
        Ingested::Late
    );
    assert_eq!(w.take_late().len(), 1);
}
