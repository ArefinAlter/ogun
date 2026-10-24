//! The gateway core: the durable ingest log (O-10), sealed segments and the sync outbox (O-11).
//!
//! The platform layer (the Android foreground service) receives sensor messages and calls [`Gateway::append`];
//! the source is acknowledged only after that returns, so an MQTT broker redelivers what the gateway did not
//! commit and HTTP push gets `503` while a `block` log is full. [`Gateway::seal`] cuts the log into segments on
//! the receive clock, writes each as a content-addressed NDJSON blob and queues it in the outbox. The sync job
//! sends [`Gateway::offer`] to the hub, uploads what the hub wants in chunks ([`Gateway::chunk`]) and applies the
//! hub's acknowledgements ([`Gateway::apply_want`]); only acknowledged segments are deleted, and only then are
//! their ingest-log records released (O-11 section 3.4).
//!
//! The gateway relays records; it does not window them by event time. The hub (or an edge running `ogund`)
//! builds the O-09 windows from the relayed records, which carry their `received_ms` and payload unchanged.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ogun_edge::{Incoming, IngestLog, Item, LogStats, Logged, Offer, Outbox, Want};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::CoreConfig;
use crate::error::{GatewayError, GatewayResult};
use crate::identity::EdgeKey;
use crate::window::{key, segment_start};

/// Bytes per `sync.chunk`: the hub assembles 512 KiB chunks (`ogun_sync::CHUNK_BYTES`, O-11 section 6), which
/// keeps one base64 chunk under the daemon's 1 MiB request line.
pub const CHUNK_BYTES: u64 = 512 * 1024;
/// Items per offer; the rest wait for the next round.
pub const MAX_OFFER_ITEMS: usize = 256;
/// Largest single payload accepted.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
/// Largest origin label.
pub const MAX_ORIGIN_BYTES: usize = 256;
/// Records read from the log per step while sealing.
const SEAL_READ_BATCH: usize = 2_000;

/// One message as the platform layer received it. The payload is text; the core parses it, so a payload that is
/// not JSON is counted as `invalid` and its bytes are never logged (O-10 section 2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRecord {
    /// The gateway's clock at receipt, Unix ms.
    pub received_ms: i64,
    /// Where it came from: `mqtt:<topic>` or `http:<peer address>`. Never a URL with credentials.
    pub origin: String,
    /// The payload text: a JSON object, or an array of objects.
    pub payload: String,
}

/// What an append did; the platform acknowledges the source only when `full` is false.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AppendReport {
    /// Records written durably.
    pub written: u64,
    /// Records refused under `drop_newest`.
    pub dropped_newest: u64,
    /// Older records evicted under `drop_oldest`.
    pub evicted_oldest: u64,
    /// Payloads that were not JSON objects; dropped and counted.
    pub invalid: u64,
    /// Sequence number of the first record written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_seq: Option<u64>,
}

/// A sealed segment waiting for the hub's acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// The outbox item.
    pub item: Item,
    /// First ingest sequence number in the segment.
    pub first_seq: u64,
    /// Last ingest sequence number in the segment.
    pub last_seq: u64,
    /// Records in the segment.
    pub records: u64,
    /// When it was sealed, Unix ms.
    pub sealed_ms: i64,
}

/// The outcome of the last sync attempt, as the platform reported it. Never a URL or a response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncOutcome {
    /// When it finished, Unix ms.
    pub at_ms: i64,
    /// Whether the round completed.
    pub ok: bool,
    /// A short reason, e.g. `http 503` or `no unmetered network`.
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    sealed_through: Option<u64>,
    pending: BTreeMap<String, Segment>,
    invalid: u64,
    acked_segments: u64,
    acked_bytes: u64,
    last_seal_ms: Option<i64>,
    last_sync: Option<SyncOutcome>,
}

/// One upload the sync job must make: chunks `from_index..total` of a blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Upload {
    /// The blob.
    pub blake3: String,
    /// Its partition key.
    pub partition: String,
    /// First chunk still missing at the hub.
    pub from_index: u32,
    /// Chunks in total.
    pub total: u32,
}

/// What [`Gateway::apply_want`] did and what is left to send.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WantPlan {
    /// Segments the hub acknowledged in this answer; deleted here.
    pub acked: u64,
    /// Ingest-log records released because every segment holding them is acknowledged.
    pub released: u64,
    /// Uploads to make, oldest first.
    pub uploads: Vec<Upload>,
}

/// What [`Gateway::seal`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SealReport {
    /// Segments sealed.
    pub segments: u64,
    /// Records in them.
    pub records: u64,
}

/// The gateway's counters, for the Studio page and the notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// Gateway label.
    pub name: String,
    /// Asset name of its segments.
    pub asset: String,
    /// The hub's `edg_` id, once enrolled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
    /// Ed25519 public key, base64url, for `edges.enroll` on the hub.
    pub public_key: String,
    /// First 16 hex digits of BLAKE3 over the key, to compare on both screens.
    pub fingerprint: String,
    /// Ingest log counters (O-10 section 6).
    pub log: LogStats,
    /// Records in the log now.
    pub stored_records: u64,
    /// Every record at or below this is in a sealed segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sealed_through: Option<u64>,
    /// Segments waiting for the hub.
    pub pending_segments: u64,
    /// Their bytes.
    pub pending_bytes: u64,
    /// Segments the hub acknowledged, all time.
    pub acked_segments: u64,
    /// Their bytes.
    pub acked_bytes: u64,
    /// Payloads dropped as not JSON, all time.
    pub invalid: u64,
    /// Last seal, Unix ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seal_ms: Option<i64>,
    /// Last sync attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<SyncOutcome>,
}

/// The gateway: one ingest log, one key, one outbox, under one directory.
#[derive(Debug)]
pub struct Gateway {
    dir: PathBuf,
    config: CoreConfig,
    log: IngestLog,
    key: EdgeKey,
    state: State,
    outbox: Outbox,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> GatewayResult<()> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    Ok(())
}

fn is_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Parses one payload into JSON objects: an object, or an array of 1 to 10 000 objects.
fn parse_payload(text: &str) -> Option<Vec<Value>> {
    if text.len() > MAX_PAYLOAD_BYTES {
        return None;
    }
    match serde_json::from_str::<Value>(text).ok()? {
        v @ Value::Object(_) => Some(vec![v]),
        Value::Array(items)
            if !items.is_empty() && items.len() <= 10_000 && items.iter().all(Value::is_object) =>
        {
            Some(items)
        }
        _ => None,
    }
}

fn clean_origin(origin: &str) -> GatewayResult<String> {
    if origin.is_empty() || origin.len() > MAX_ORIGIN_BYTES || origin.contains('@') {
        return Err(GatewayError::Invalid(
            "origin must be a short label without credentials".into(),
        ));
    }
    Ok(origin.chars().filter(|c| !c.is_control()).collect())
}

fn chunk_count(size: u64) -> u32 {
    u32::try_from(size.div_ceil(CHUNK_BYTES).max(1)).unwrap_or(u32::MAX)
}

impl Gateway {
    /// Opens (or creates) the gateway under `dir`.
    ///
    /// # Errors
    ///
    /// A bad configuration, a log held by another process, or storage errors.
    pub fn open(dir: &Path, config: CoreConfig) -> GatewayResult<Self> {
        config.validate()?;
        fs::create_dir_all(dir.join("blobs"))?;
        let log = IngestLog::open(
            &dir.join("ingest.redb"),
            config.max_log_bytes,
            config.when_full,
        )?;
        let key = EdgeKey::load_or_create(&dir.join("edge.key"))?;
        let state: State = match fs::read(dir.join("state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| GatewayError::Storage("the gateway state file is corrupt".into()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        let mut outbox = Outbox::default();
        for seg in state.pending.values() {
            outbox.push(seg.item.clone());
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            config,
            log,
            key,
            state,
            outbox,
        })
    }

    /// The configuration in use.
    #[must_use]
    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    fn save(&self) -> GatewayResult<()> {
        let bytes =
            serde_json::to_vec(&self.state).map_err(|e| GatewayError::Storage(e.to_string()))?;
        write_atomic(&self.dir.join("state.json"), &bytes)
    }

    fn blob_path(&self, digest: &str) -> PathBuf {
        self.dir.join("blobs").join(digest)
    }

    /// Appends received messages in one durable transaction. Invalid payloads are counted and dropped.
    ///
    /// # Errors
    ///
    /// [`GatewayError::Full`] under `block` (nothing written: do not acknowledge), or storage errors.
    pub fn append(&mut self, records: Vec<RawRecord>) -> GatewayResult<AppendReport> {
        let mut batch = Vec::with_capacity(records.len());
        let mut invalid = 0u64;
        for r in records {
            let origin = clean_origin(&r.origin)?;
            match parse_payload(&r.payload) {
                Some(objects) => batch.extend(objects.into_iter().map(|payload| Incoming {
                    received_ms: r.received_ms,
                    origin: origin.clone(),
                    payload,
                })),
                None => invalid += 1,
            }
        }
        self.finish_append(batch, invalid)
    }

    /// Appends an HTTP push body: JSON (an object or an array of objects) or NDJSON (one object per line).
    ///
    /// # Errors
    ///
    /// As [`Gateway::append`].
    pub fn append_body(
        &mut self,
        origin: &str,
        received_ms: i64,
        body: &str,
        ndjson: bool,
    ) -> GatewayResult<AppendReport> {
        let origin = clean_origin(origin)?;
        let mut batch = Vec::new();
        let mut invalid = 0u64;
        let lines: Vec<&str> = if ndjson {
            body.lines().filter(|l| !l.trim().is_empty()).collect()
        } else {
            vec![body]
        };
        for line in lines {
            match parse_payload(line) {
                Some(objects) => batch.extend(objects.into_iter().map(|payload| Incoming {
                    received_ms,
                    origin: origin.clone(),
                    payload,
                })),
                None => invalid += 1,
            }
        }
        self.finish_append(batch, invalid)
    }

    fn finish_append(&mut self, batch: Vec<Incoming>, invalid: u64) -> GatewayResult<AppendReport> {
        let appended = if batch.is_empty() {
            ogun_edge::Appended::default()
        } else {
            self.log.append(batch)?
        };
        if invalid > 0 {
            self.state.invalid += invalid;
            self.save()?;
        }
        Ok(AppendReport {
            written: appended.written,
            dropped_newest: appended.dropped_newest,
            evicted_oldest: appended.evicted_oldest,
            invalid,
            first_seq: appended.first_seq,
        })
    }

    /// Seals every complete segment: its receive-clock span plus the grace period has passed, or it reached
    /// `segment_max_records`. Segments are sealed in sequence order and never skip one, so `sealed_through` is
    /// a frontier.
    ///
    /// # Errors
    ///
    /// Storage errors; segments sealed before the error stay sealed.
    pub fn seal(&mut self, now_ms: i64) -> GatewayResult<SealReport> {
        let size = self.config.segment_ms;
        let wait = i64::try_from(size + self.config.grace_ms).unwrap_or(i64::MAX);
        let max = usize::try_from(self.config.segment_max_records).unwrap_or(usize::MAX);
        let closed = |start: i64, len: usize| len >= max || start.saturating_add(wait) <= now_ms;
        let mut report = SealReport::default();
        let mut from = self.state.sealed_through.map_or(0, |s| s + 1);
        let mut group: Option<(i64, Vec<Logged>)> = None;
        'read: loop {
            let batch = self.log.read(from, SEAL_READ_BATCH)?;
            if batch.is_empty() {
                break;
            }
            for rec in batch {
                from = rec.seq + 1;
                let start = segment_start(rec.record.received_ms, size);
                group = match group.take() {
                    Some((s, mut recs)) if s == start && recs.len() < max => {
                        recs.push(rec);
                        Some((s, recs))
                    }
                    Some((s, recs)) => {
                        if !closed(s, recs.len()) {
                            break 'read;
                        }
                        report.records += recs.len() as u64;
                        self.seal_group(s, recs, now_ms)?;
                        report.segments += 1;
                        Some((start, vec![rec]))
                    }
                    None => Some((start, vec![rec])),
                };
            }
        }
        if let Some((s, recs)) = group
            && closed(s, recs.len())
            && self.state.sealed_through.is_none_or(|t| recs[0].seq > t)
        {
            report.records += recs.len() as u64;
            self.seal_group(s, recs, now_ms)?;
            report.segments += 1;
        }
        self.state.last_seal_ms = Some(now_ms);
        self.save()?;
        Ok(report)
    }

    fn seal_group(&mut self, start: i64, recs: Vec<Logged>, now_ms: i64) -> GatewayResult<()> {
        let (Some(first), Some(last)) = (recs.first(), recs.last()) else {
            return Ok(());
        };
        let (first_seq, last_seq) = (first.seq, last.seq);
        let mut bytes = Vec::new();
        for r in &recs {
            serde_json::to_writer(&mut bytes, r)
                .map_err(|e| GatewayError::Storage(e.to_string()))?;
            bytes.push(b'\n');
        }
        let digest = blake3::hash(&bytes).to_hex().to_string();
        write_atomic(&self.blob_path(&digest), &bytes)?;
        let item = Item {
            asset: self.config.asset(),
            partition: format!("{}-s{first_seq}", key(start)),
            revision: 1,
            blake3: digest.clone(),
            size_bytes: bytes.len() as u64,
        };
        self.outbox.push(item.clone());
        self.state.pending.insert(
            digest,
            Segment {
                item,
                first_seq,
                last_seq,
                records: recs.len() as u64,
                sealed_ms: now_ms,
            },
        );
        self.state.sealed_through = Some(last_seq);
        self.save()
    }

    /// The signed `sync.offer` body (`ogun_api::methods::SyncOfferParams`), oldest segments first, or `None`
    /// when nothing waits. The signature is Ed25519 over the JCS form of `offer`.
    ///
    /// # Errors
    ///
    /// [`GatewayError::NotEnrolled`] before the hub's `edg_` id is set.
    pub fn offer(&self) -> GatewayResult<Option<Value>> {
        let edge_id = self
            .config
            .edge_id
            .clone()
            .ok_or(GatewayError::NotEnrolled)?;
        let mut segments: Vec<&Segment> = self.state.pending.values().collect();
        if segments.is_empty() {
            return Ok(None);
        }
        segments.sort_by_key(|s| s.first_seq);
        let offer = Offer {
            edge_id: edge_id.clone(),
            items: segments
                .into_iter()
                .take(MAX_OFFER_ITEMS)
                .map(|s| s.item.clone())
                .collect(),
        };
        let jcs = zeus_verify::canonical::to_jcs_bytes(&offer)
            .map_err(|e| GatewayError::Storage(e.to_string()))?;
        let signature = self.key.sign(&jcs);
        Ok(Some(serde_json::json!({
            "edge_id": edge_id,
            "offer": offer,
            "signature": signature,
        })))
    }

    /// Applies the hub's answer: deletes acknowledged segments, releases the ingest-log records no pending segment
    /// still holds, and returns the uploads the hub wants.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn apply_want(&mut self, want: &Want) -> GatewayResult<WantPlan> {
        let mut plan = WantPlan::default();
        let acked: BTreeSet<String> = want
            .acked
            .iter()
            .filter(|d| is_digest(d))
            .cloned()
            .collect();
        for item in self.outbox.ack(&acked) {
            if let Some(seg) = self.state.pending.remove(&item.blake3) {
                self.state.acked_segments += 1;
                self.state.acked_bytes += seg.item.size_bytes;
                plan.acked += 1;
            }
            match fs::remove_file(self.blob_path(&item.blake3)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let frontier = match self.state.pending.values().map(|s| s.first_seq).min() {
            Some(0) => None,
            Some(first) => Some(first - 1),
            None => self.state.sealed_through,
        };
        if let Some(f) = frontier
            && self.log.stats()?.released_through.is_none_or(|r| f > r)
        {
            plan.released = self.log.release_through(f)?;
        }
        let mut uploads: Vec<(u64, Upload)> = want
            .blobs
            .iter()
            .filter_map(|(digest, offset)| {
                let seg = self.state.pending.get(digest)?;
                let total = chunk_count(seg.item.size_bytes);
                let from = u32::try_from(offset / CHUNK_BYTES)
                    .unwrap_or(u32::MAX)
                    .min(total);
                (from < total).then(|| {
                    (
                        seg.first_seq,
                        Upload {
                            blake3: digest.clone(),
                            partition: seg.item.partition.clone(),
                            from_index: from,
                            total,
                        },
                    )
                })
            })
            .collect();
        uploads.sort_by_key(|(first, _)| *first);
        plan.uploads = uploads.into_iter().map(|(_, u)| u).collect();
        self.save()?;
        Ok(plan)
    }

    /// One `sync.chunk` body (`ogun_api::methods::SyncChunkParams`).
    ///
    /// # Errors
    ///
    /// [`GatewayError::NotFound`] for a digest not pending, [`GatewayError::Invalid`] for an index past the end,
    /// [`GatewayError::NotEnrolled`] before enrolment, or storage errors.
    pub fn chunk(&self, digest: &str, index: u32) -> GatewayResult<Value> {
        let edge_id = self
            .config
            .edge_id
            .clone()
            .ok_or(GatewayError::NotEnrolled)?;
        let seg =
            self.state.pending.get(digest).ok_or_else(|| {
                GatewayError::NotFound("no pending segment with that digest".into())
            })?;
        let total = chunk_count(seg.item.size_bytes);
        if index >= total {
            return Err(GatewayError::Invalid("chunk index past the end".into()));
        }
        let offset = u64::from(index) * CHUNK_BYTES;
        let len = CHUNK_BYTES.min(seg.item.size_bytes.saturating_sub(offset));
        let mut f = fs::File::open(self.blob_path(digest))?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
        f.read_exact(&mut buf)?;
        Ok(serde_json::json!({
            "edge_id": edge_id,
            "partition": seg.item.partition,
            "blake3": digest,
            "index": index,
            "total": total,
            "data_base64": STANDARD.encode(&buf),
        }))
    }

    /// Records how the last sync round ended.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn record_sync(&mut self, outcome: SyncOutcome) -> GatewayResult<()> {
        let mut outcome = outcome;
        outcome.reason = outcome
            .reason
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect();
        self.state.last_sync = Some(outcome);
        self.save()
    }

    /// The counters.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn status(&self) -> GatewayResult<Status> {
        Ok(Status {
            name: self.config.name.clone(),
            asset: self.config.asset(),
            edge_id: self.config.edge_id.clone(),
            public_key: self.key.public_key(),
            fingerprint: self.key.fingerprint(),
            log: self.log.stats()?,
            stored_records: self.log.len()?,
            sealed_through: self.state.sealed_through,
            pending_segments: self.state.pending.len() as u64,
            pending_bytes: self.state.pending.values().map(|s| s.item.size_bytes).sum(),
            acked_segments: self.state.acked_segments,
            acked_bytes: self.state.acked_bytes,
            invalid: self.state.invalid,
            last_seal_ms: self.state.last_seal_ms,
            last_sync: self.state.last_sync.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn rec(ms: i64, n: i64) -> RawRecord {
        RawRecord {
            received_ms: ms,
            origin: "mqtt:site-a/p1/telemetry".into(),
            payload: format!(r#"{{"device_id":"p1","seq":{n},"pm25_ugm3_x10":351}}"#),
        }
    }

    fn enrolled(dir: &Path) -> Gateway {
        let mut c = CoreConfig::named("phone");
        c.edge_id = Some("edg_01JB0000000000000000000000".into());
        Gateway::open(dir, c).unwrap()
    }

    #[test]
    fn append_counts_invalid_payloads_and_seals_closed_segments_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = enrolled(dir.path());
        let t0 = 1_792_743_300_000; // a segment boundary
        let mut batch = vec![rec(t0 + 1_000, 1), rec(t0 + 2_000, 2)];
        batch.push(RawRecord {
            received_ms: t0,
            origin: "mqtt:x".into(),
            payload: "not json".into(),
        });
        let r = g.append(batch).unwrap();
        assert_eq!((r.written, r.invalid), (2, 1));
        g.append(vec![rec(t0 + 300_000 + 5, 3)]).unwrap();
        // The first segment is closed only after its end plus the grace period.
        assert_eq!(g.seal(t0 + 300_000 + 10).unwrap().segments, 0);
        let s = g.seal(t0 + 330_000).unwrap();
        assert_eq!((s.segments, s.records), (1, 2));
        let st = g.status().unwrap();
        assert_eq!(st.sealed_through, Some(1));
        assert_eq!(st.pending_segments, 1);
        assert_eq!(st.invalid, 1);
    }

    #[test]
    fn sync_round_uploads_wanted_chunks_and_releases_after_ack() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = enrolled(dir.path());
        let t0 = 1_792_743_300_000;
        g.append(vec![rec(t0, 1), rec(t0 + 1, 2)]).unwrap();
        g.seal(t0 + 400_000).unwrap();
        let offer = g.offer().unwrap().unwrap();
        let items = offer["offer"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0]["partition"].as_str().unwrap().starts_with("w"));
        let digest = items[0]["blake3"].as_str().unwrap().to_owned();
        let want = Want {
            blobs: BTreeMap::from([(digest.clone(), 0)]),
            acked: BTreeSet::new(),
        };
        let plan = g.apply_want(&want).unwrap();
        assert_eq!(plan.uploads.len(), 1);
        let chunk = g.chunk(&digest, 0).unwrap();
        let bytes = STANDARD
            .decode(chunk["data_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(blake3::hash(&bytes).to_hex().as_str(), digest);
        assert_eq!(g.status().unwrap().stored_records, 2);
        let ack = Want {
            blobs: BTreeMap::new(),
            acked: BTreeSet::from([digest]),
        };
        let plan = g.apply_want(&ack).unwrap();
        assert_eq!((plan.acked, plan.released), (1, 2));
        assert!(g.offer().unwrap().is_none());
        assert_eq!(g.status().unwrap().stored_records, 0);
    }

    #[test]
    fn state_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = 1_792_743_300_000;
        {
            let mut g = enrolled(dir.path());
            g.append(vec![rec(t0, 1)]).unwrap();
            g.seal(t0 + 400_000).unwrap();
        }
        let g = enrolled(dir.path());
        assert_eq!(g.status().unwrap().pending_segments, 1);
        assert!(g.offer().unwrap().is_some());
    }

    #[test]
    fn offers_need_enrolment_and_origins_reject_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = Gateway::open(dir.path(), CoreConfig::named("phone")).unwrap();
        assert!(matches!(g.offer(), Err(GatewayError::NotEnrolled)));
        let bad = RawRecord {
            received_ms: 0,
            origin: "mqtt://user:pw@broker".into(),
            payload: "{}".into(),
        };
        assert!(g.append(vec![bad]).is_err());
        let r = g
            .append_body(
                "http:192.168.1.20",
                5,
                "{\"a\":1}\n\nnope\n{\"b\":2}\n",
                true,
            )
            .unwrap();
        assert_eq!((r.written, r.invalid), (2, 1));
    }
}
