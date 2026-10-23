//! Deterministic sealed batches (O-09 section 6).
//!
//! A sealed window is JSON Lines: one JCS-canonical object per record, sorted by
//! `(key, event_ms, seq)`, each line ending in `\n`, with no timestamps of its own. The same
//! records therefore always produce the same bytes and the same BLAKE3 digest, so re-sealing after
//! a crash, or re-sealing because of a duplicate late record, is recognised as the same revision.
//! Columnar Parquet output is a later operator over these batches; its writer version would have
//! to be pinned to keep that guarantee.

use serde::Serialize;
use zeus_verify::canonical::to_jcs_bytes;

use crate::window::Record;

/// The bytes and digest of one sealed revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedBatch {
    /// JSON Lines, canonical.
    pub bytes: Vec<u8>,
    /// BLAKE3 of `bytes`, 64 lowercase hex characters.
    pub blake3: String,
    /// Records in the batch.
    pub records: usize,
}

#[derive(Serialize)]
struct Line<'a> {
    key: &'a str,
    event_ms: i64,
    seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    dedupe: Option<&'a str>,
    payload: &'a serde_json::Value,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    clamped: bool,
}

/// Serialises records into a sealed batch. Sorts them, so the input order does not matter.
///
/// # Errors
///
/// Only if a payload cannot be canonicalised (for example a non-finite number, which JSON parsing
/// never produces).
pub fn seal(records: &[Record]) -> Result<SealedBatch, zeus_verify::VerifyError> {
    let mut sorted: Vec<&Record> = records.iter().collect();
    sorted.sort_by(|a, b| (&a.key, a.event_ms, a.seq).cmp(&(&b.key, b.event_ms, b.seq)));
    let mut bytes = Vec::new();
    for r in sorted {
        let line = Line {
            key: &r.key,
            event_ms: r.event_ms,
            seq: r.seq,
            dedupe: r.dedupe.as_deref(),
            payload: &r.payload,
            clamped: r.clamped,
        };
        bytes.extend(to_jcs_bytes(&line)?);
        bytes.push(b'\n');
    }
    let blake3 = blake3::hash(&bytes).to_hex().to_string();
    Ok(SealedBatch {
        bytes,
        blake3,
        records: records.len(),
    })
}

/// The partition key of a window: `w` and the window start as `YYYYMMDDTHHMMSSZ` in UTC
/// (O-09 section 2). It matches the static-key pattern of O-02 and sorts in time order.
#[must_use]
pub fn window_key(start_ms: i64) -> String {
    ogun_sched::civil::format_window_key(start_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(key: &str, event_ms: i64, seq: u64, pm: f64) -> Record {
        Record {
            key: key.into(),
            event_ms,
            seq,
            dedupe: None,
            payload: json!({ "pm25": pm }),
            clamped: false,
        }
    }

    #[test]
    fn same_records_same_digest_in_any_order() {
        let a = vec![
            rec("d1", 10, 1, 35.5),
            rec("d2", 5, 2, 80.25),
            rec("d1", 7, 3, 12.0),
        ];
        let mut b = a.clone();
        b.reverse();
        let (sa, sb) = (seal(&a).unwrap(), seal(&b).unwrap());
        assert_eq!(sa, sb);
        let text = String::from_utf8(sa.bytes).unwrap();
        assert!(
            text.starts_with(r#"{"event_ms":7,"key":"d1","payload":{"pm25":12},"seq":3}"#),
            "{text}"
        );
        assert_eq!(text.lines().count(), 3);
    }

    #[test]
    fn any_change_changes_the_digest() {
        let a = seal(&[rec("d1", 10, 1, 35.5)]).unwrap();
        let b = seal(&[rec("d1", 10, 1, 35.6)]).unwrap();
        assert_ne!(a.blake3, b.blake3);
    }

    #[test]
    fn window_keys_sort_and_match_the_key_pattern() {
        assert_eq!(window_key(1_792_743_300_000), "w20261023T081500Z");
        assert_eq!(window_key(0), "w19700101T000000Z");
        assert!(window_key(60_000) < window_key(120_000));
    }
}
