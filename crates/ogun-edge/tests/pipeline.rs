//! An edge site end to end, without a network: sensor messages land in the durable ingest log,
//! become windows, are sealed, offered to a hub that verifies them, acknowledged, and only then
//! released from the log. Then the same thing with the edge restarted between seal and ack.

use std::collections::{BTreeMap, BTreeSet};

use ogun_edge::{Incoming, IngestLog, Item, LogError, Outbox, chunks, to_record, verify, want};
use ogun_ir::model::{WhenFull, WindowPartitions};
use ogun_stream::{Action, WindowSpec, Windower, seal, window_key};
use serde_json::json;

const MIN: i64 = 60_000;

fn window() -> WindowPartitions {
    WindowPartitions {
        source: "sensors".into(),
        time_field: "ts_ms".into(),
        key_field: "device_id".into(),
        size_ms: 60_000,
        allowed_lateness_ms: 0,
        reseal_after_ms: 30_000,
        max_delay_ms: 10_000,
        idle_timeout_ms: 600_000,
        max_future_ms: 300_000,
    }
}

fn spec(w: &WindowPartitions) -> WindowSpec {
    WindowSpec {
        size_ms: w.size_ms,
        allowed_lateness_ms: w.allowed_lateness_ms,
        reseal_after_ms: w.reseal_after_ms,
        max_delay_ms: w.max_delay_ms,
        idle_timeout_ms: w.idle_timeout_ms,
        max_future_ms: w.max_future_ms,
    }
}

fn reading(device: &str, seq: u64, ts: i64, pm25_x10: i64) -> Incoming {
    Incoming {
        received_ms: ts + 500,
        origin: format!("site-a/{device}/telemetry"),
        payload: json!({ "device_id": device, "seq": seq, "ts_ms": ts, "pm25_ugm3_x10": pm25_x10, "filter_dp_pa": 112 }),
    }
}

#[test]
fn sensor_to_hub_and_back_to_an_empty_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = IngestLog::open(&dir.path().join("ingest.redb"), 1 << 20, WhenFull::Block).unwrap();
    let t0 = 1000 * MIN;

    // Two purifiers and an outdoor monitor publish for two minutes, plus one duplicate.
    let mut batch = Vec::new();
    for (i, s) in (0..12).map(|s| (s, s as i64 * 10_000)) {
        batch.push(reading("purifier-1", i, t0 + s, 351));
        batch.push(reading("purifier-2", i, t0 + s, 280));
        batch.push(reading("outdoor-1", i, t0 + s, 1102));
    }
    batch.push(reading("purifier-1", 0, t0, 351));
    let appended = log.append(batch).unwrap();
    assert_eq!(appended.written, 37);

    let w = window();
    let dedupe = vec!["device_id".to_owned(), "seq".to_owned()];
    let mut windower = Windower::new(spec(&w)).unwrap();
    for logged in log.read(0, 1000).unwrap() {
        let record = to_record(&logged, &w, &dedupe).unwrap();
        windower.ingest(record, logged.record.received_ms);
    }
    assert_eq!(windower.counters().duplicates, 1);

    let mut outbox = Outbox::default();
    let mut blobs = BTreeMap::new();
    // While the devices are active, only the first window is behind the watermark; once they go
    // quiet past the idle timeout, the watermark follows the clock and the second one seals too.
    let mut actions = windower.poll(t0 + 2 * MIN + 10_000);
    assert_eq!(actions.len(), 1);
    actions.extend(windower.poll(t0 + 12 * MIN));
    for action in actions {
        if let Action::Seal {
            window_start,
            revision,
            records,
        } = action
        {
            let batch = seal(&records).unwrap();
            windower.seal_committed(window_start, &batch.blake3);
            outbox.push(Item {
                asset: "air-quality-stream/sensors".into(),
                partition: window_key(window_start),
                revision,
                blake3: batch.blake3.clone(),
                size_bytes: batch.bytes.len() as u64,
            });
            blobs.insert(batch.blake3, batch.bytes);
        }
    }
    assert_eq!(outbox.pending(), 2, "two one-minute windows sealed");

    // The hub already has nothing; it wants both, receives them in chunks and verifies them.
    let offer = outbox.offer("edg_01M45MN5M0PZ0D9W0APDCGRNZD");
    let w1 = want(&offer, &BTreeSet::new(), &BTreeMap::new());
    let mut hub = BTreeSet::new();
    for item in &offer.items {
        let bytes = &blobs[&item.blake3];
        let mut received = Vec::new();
        for c in chunks(item.size_bytes, w1.blobs[&item.blake3], 512) {
            received.extend_from_slice(&bytes[c.offset as usize..(c.offset + c.len) as usize]);
        }
        verify(item, &received).unwrap();
        hub.insert(item.blake3.clone());
    }
    // The next offer is answered with acknowledgements only.
    let w2 = want(
        &outbox.offer("edg_01M45MN5M0PZ0D9W0APDCGRNZD"),
        &hub,
        &BTreeMap::new(),
    );
    assert!(w2.blobs.is_empty());
    assert_eq!(outbox.ack(&w2.acked).len(), 2);

    // Windows are final (lateness 0) and synced: the log can release everything.
    let last = log.read(0, 1000).unwrap().last().unwrap().seq;
    assert_eq!(log.release_through(last).unwrap(), 37);
    assert!(log.is_empty().unwrap());
    assert_eq!(log.stats().unwrap().bytes_used, 0);
}

#[test]
fn a_full_log_blocks_or_drops_by_policy() {
    let dir = tempfile::tempdir().unwrap();
    let one = serde_json::to_vec(&reading("p", 0, 0, 1)).unwrap().len() as u64;

    let block = IngestLog::open(&dir.path().join("b.redb"), one * 3, WhenFull::Block).unwrap();
    block
        .append((0..3).map(|i| reading("p", i, 0, 1)).collect())
        .unwrap();
    assert!(matches!(
        block.append(vec![reading("p", 3, 0, 1)]),
        Err(LogError::Full { .. })
    ));
    assert_eq!(block.len().unwrap(), 3);

    let newest =
        IngestLog::open(&dir.path().join("n.redb"), one * 3, WhenFull::DropNewest).unwrap();
    let a = newest
        .append((0..5).map(|i| reading("p", i, 0, 1)).collect())
        .unwrap();
    assert_eq!((a.written, a.dropped_newest), (3, 2));

    let oldest =
        IngestLog::open(&dir.path().join("o.redb"), one * 3, WhenFull::DropOldest).unwrap();
    oldest
        .append((0..3).map(|i| reading("p", i, 0, 1)).collect())
        .unwrap();
    oldest.release_through(0).unwrap();
    let a = oldest
        .append((3..5).map(|i| reading("p", i, 0, 1)).collect())
        .unwrap();
    assert_eq!(a.written, 2);
    let stats = oldest.stats().unwrap();
    assert_eq!(stats.lost_oldest, 1, "one unreleased record had to go");
    let seqs: Vec<u64> = oldest.read(0, 10).unwrap().iter().map(|l| l.seq).collect();
    assert_eq!(seqs, vec![2, 3, 4]);
}

#[test]
fn the_log_survives_reopening_with_sequences_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ingest.redb");
    {
        let log = IngestLog::open(&path, 1 << 20, WhenFull::Block).unwrap();
        log.append(vec![reading("p", 0, 0, 1), reading("p", 1, 0, 1)])
            .unwrap();
    }
    let log = IngestLog::open(&path, 1 << 20, WhenFull::Block).unwrap();
    let a = log.append(vec![reading("p", 2, 0, 1)]).unwrap();
    assert_eq!(a.first_seq, Some(2));
    assert_eq!(log.len().unwrap(), 3);
}

#[test]
fn records_without_time_or_key_are_refused() {
    let w = window();
    let bad = ogun_edge::Logged {
        seq: 0,
        record: Incoming {
            received_ms: 0,
            origin: "x".into(),
            payload: json!({ "device_id": "d" }),
        },
    };
    assert!(to_record(&bad, &w, &[]).is_err());
    let bad = ogun_edge::Logged {
        seq: 0,
        record: Incoming {
            received_ms: 0,
            origin: "x".into(),
            payload: json!({ "ts_ms": 5 }),
        },
    };
    assert!(to_record(&bad, &w, &[]).is_err());
}
