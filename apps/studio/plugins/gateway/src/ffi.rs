//! A string-in, string-out facade over one process-wide [`Gateway`], for the platform layer.
//!
//! The Android service, the sync job and the Studio plugin run in one process and share this gateway (redb opens
//! a file once per process). Every function takes and returns JSON text: `{"ok": <value>}` on success,
//! `{"error": <O-00 envelope>}` on failure. The JNI bridge (`android.rs`) is a thin shim over these functions, so
//! all of the logic is safe Rust that the desktop test suite exercises.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use ogun_edge::Want;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::config::CoreConfig;
use crate::core::{Gateway, RawRecord, SyncOutcome};
use crate::error::{GatewayError, GatewayResult};

static GATEWAY: Mutex<Option<Gateway>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Gateway>> {
    GATEWAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn respond<T: Serialize>(result: GatewayResult<T>) -> String {
    let body = match result {
        Ok(v) => json!({ "ok": v }),
        Err(e) => json!({ "error": e.envelope() }),
    };
    body.to_string()
}

fn parse<T: DeserializeOwned>(text: &str) -> GatewayResult<T> {
    serde_json::from_str(text).map_err(|e| {
        GatewayError::Invalid(format!(
            "bad JSON at line {} column {}",
            e.line(),
            e.column()
        ))
    })
}

fn with<T>(f: impl FnOnce(&mut Gateway) -> GatewayResult<T>) -> GatewayResult<T> {
    let mut guard = lock();
    let gw = guard.as_mut().ok_or(GatewayError::NotOpen)?;
    f(gw)
}

/// Opens the gateway under `dir` with `config_json` (a [`CoreConfig`]). Reopens when the configuration changed;
/// a no-op when it is already open with the same configuration. Returns the status.
#[must_use]
pub fn open(dir: &str, config_json: &str) -> String {
    respond((|| {
        let config: CoreConfig = parse(config_json)?;
        let mut guard = lock();
        let same = guard.as_ref().is_some_and(|g| g.config() == &config);
        if !same {
            // Drop the old handle first: redb refuses a second open of the same file in one process.
            *guard = None;
            *guard = Some(Gateway::open(Path::new(dir), config)?);
        }
        guard.as_ref().ok_or(GatewayError::NotOpen)?.status()
    })())
}

/// Closes the gateway (the service stopped and no job runs).
#[must_use]
pub fn close() -> String {
    *lock() = None;
    respond(Ok(Value::Null))
}

/// Appends `{"records": [RawRecord, ...]}` (MQTT messages).
#[must_use]
pub fn append(batch_json: &str) -> String {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Batch {
        records: Vec<RawRecord>,
    }
    respond(parse::<Batch>(batch_json).and_then(|b| with(|g| g.append(b.records))))
}

/// Appends one HTTP push body.
#[must_use]
pub fn append_body(origin: &str, received_ms: i64, body: &str, ndjson: bool) -> String {
    respond(with(|g| g.append_body(origin, received_ms, body, ndjson)))
}

/// Seals complete segments.
#[must_use]
pub fn seal(now_ms: i64) -> String {
    respond(with(|g| g.seal(now_ms)))
}

/// The signed offer body, or `null` when nothing waits.
#[must_use]
pub fn offer() -> String {
    respond(with(|g| g.offer()))
}

/// Applies the hub's `want` (the `want` member of the `sync.offer` result) and returns the upload plan.
#[must_use]
pub fn apply_want(want_json: &str) -> String {
    respond(parse::<Want>(want_json).and_then(|w| with(|g| g.apply_want(&w))))
}

/// One chunk body.
#[must_use]
pub fn chunk(digest: &str, index: u32) -> String {
    respond(with(|g| g.chunk(digest, index)))
}

/// Records the outcome of a sync round (`{"at_ms", "ok", "reason"}`).
#[must_use]
pub fn record_sync(outcome_json: &str) -> String {
    respond(parse::<SyncOutcome>(outcome_json).and_then(|o| with(|g| g.record_sync(o))))
}

/// The counters.
#[must_use]
pub fn status() -> String {
    respond(with(|g| g.status()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_round_trip_and_errors_are_envelopes() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_str().unwrap();
        let before: Value = serde_json::from_str(&status()).unwrap();
        assert!(before.get("error").is_some() || before.get("ok").is_some());
        let opened: Value = serde_json::from_str(&open(d, r#"{"name":"phone"}"#)).unwrap();
        assert!(opened["ok"]["public_key"].is_string());
        let again: Value = serde_json::from_str(&open(d, r#"{"name":"phone"}"#)).unwrap();
        assert_eq!(opened, again);
        let appended: Value = serde_json::from_str(&append(
            r#"{"records":[{"received_ms":1,"origin":"mqtt:t","payload":"{\"a\":1}"}]}"#,
        ))
        .unwrap();
        assert_eq!(appended["ok"]["written"], 1);
        let offered: Value = serde_json::from_str(&offer()).unwrap();
        assert_eq!(offered["error"]["code"], "edge.not_enrolled");
        let bad: Value = serde_json::from_str(&append("{")).unwrap();
        assert_eq!(bad["error"]["code"], "api.invalid_params");
        let _ = close();
    }
}
