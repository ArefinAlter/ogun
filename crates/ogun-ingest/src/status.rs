//! Per-source counters, logged as a status line (O-10 section 6). The bridge has no status
//! listener of its own: one daemon, one listener, and `stream.http` only where configured.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counters of one source since the bridge started.
#[derive(Debug, Default)]
pub struct Counters {
    /// Messages or requests received from the source.
    pub messages: AtomicU64,
    /// Records refused as invalid (unparseable, no key, bad time, too large). Never logged.
    pub invalid: AtomicU64,
    /// Records committed to the bridge's ingest log.
    pub written: AtomicU64,
    /// Records refused by `drop_newest`.
    pub dropped_newest: AtomicU64,
    /// Appends refused because the log was full under `block` or the disk failed.
    pub refused: AtomicU64,
    /// Records `ogund` accepted (`accepted_through`).
    pub forwarded: AtomicU64,
    /// Records `ogund` dropped by its own `when_full` policy.
    pub daemon_dropped: AtomicU64,
    /// Records `ogund` routed to `late`.
    pub daemon_late: AtomicU64,
    /// Failed `streams.push` calls.
    pub push_errors: AtomicU64,
    /// Protocol-level failures (connection lost, poll error, frame-counter gap).
    pub source_errors: AtomicU64,
    /// LoRaWAN uplinks missing by frame counter.
    pub gaps: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl Counters {
    /// Adds to a counter.
    pub fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    /// Reads a counter.
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// Records the last error. Callers pass messages without secrets or URL queries.
    pub fn set_error(&self, message: impl Into<String>) {
        if let Ok(mut e) = self.last_error.lock() {
            *e = Some(message.into());
        }
    }

    /// The last error.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|e| e.clone())
    }
}
