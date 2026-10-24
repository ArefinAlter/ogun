//! Ogun on edge devices (Ogun specs O-10 and O-11; ADR 0008).
//!
//! An edge device (a Raspberry Pi, a gateway, a site server) runs `ogund` with the `ogun-ingest`
//! bridge. Sensors and air purifiers publish over MQTT, HTTP push or LoRaWAN; the bridge appends
//! their records to a durable [`log::IngestLog`] and acknowledges only after the append commits;
//! the windower (`ogun-stream`) seals windows into content-addressed batches; the
//! [`sync::Outbox`] offers them to the hub when the link is up. Microcontrollers such as the ESP32
//! are producers only: they publish to the edge and never run Ogun.

#![forbid(unsafe_code)]

pub mod log;
pub mod sync;

use ogun_ir::model::WindowPartitions;
use ogun_stream::Record;
use serde_json::Value;

pub use log::{Appended, Incoming, IngestLog, LogError, LogStats, Logged};
pub use sync::{Chunk, Item, Offer, Outbox, VerifyError, Want, chunks, verify, want};

/// Why a logged record cannot enter a window.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The time field is missing or not an integer.
    #[error("record has no integer time field {0}")]
    Time(String),
    /// The key field is missing or not a string or integer.
    #[error("record has no key field {0}")]
    Key(String),
}

fn field<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(payload, |v, seg| v.get(seg))
}

/// Builds a window record from a logged record, using the workflow's window and dedupe settings.
/// A record that fails here goes to the stream's `invalid` count, never into a window.
///
/// # Errors
///
/// [`RecordError`] when the time or key field is unusable.
pub fn to_record(
    logged: &Logged,
    window: &WindowPartitions,
    dedupe_key: &[String],
) -> Result<Record, RecordError> {
    let payload = &logged.record.payload;
    let event_ms = field(payload, &window.time_field)
        .and_then(Value::as_i64)
        .ok_or_else(|| RecordError::Time(window.time_field.clone()))?;
    let key = match field(payload, &window.key_field) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => return Err(RecordError::Key(window.key_field.clone())),
    };
    let dedupe = if dedupe_key.is_empty() {
        None
    } else {
        // JSON arrays retain component boundaries and scalar types, unlike delimiter joins.
        let parts: Option<Vec<Value>> = dedupe_key
            .iter()
            .map(|f| field(payload, f).cloned())
            .collect();
        parts.map(|p| Value::Array(p).to_string())
    };
    Ok(Record {
        key,
        event_ms,
        seq: logged.seq,
        dedupe,
        payload: payload.clone(),
        clamped: false,
    })
}
