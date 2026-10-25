//! The forwarder: moves records from a source's ingest log into `ogund` with `streams.push`
//! (O-10 section 2.3, O-15 section 5).
//!
//! * A batch is the oldest records in the log, contiguous by sequence number, at most 10 000
//!   records and about 768 KiB of JSON (the daemon's request line is 1 MiB).
//! * `first_seq` is the log sequence of the first record. The daemon drops records at or below
//!   the last `accepted_through` it returned, so resending after a lost response duplicates
//!   nothing.
//! * Records at or below `accepted_through` are deleted from the bridge's log: they are durable in
//!   the daemon now.
//! * `retry_after_ms` (the daemon's buffer is full under `block`) and the `stream.full` error
//!   pause the forwarder; the bridge's own log keeps absorbing records meanwhile.
//! * When `ogund` restarts, the connection fails with `api.unavailable`; the forwarder reconnects
//!   with exponential back-off (1 s doubling to 60 s).

use std::sync::Arc;
use std::time::Duration;

use ogun_api::methods::{ClientInfo, PushParams, PushResult, StreamRecord, name};
use ogun_api::{ApiError, Client, OgunPaths};
use ogun_edge::to_record;
use tokio::sync::watch;

use crate::applog::LogHandle;
use crate::spec::SourceSpec;
use crate::status::Counters;

/// Most records per `streams.push` (O-15 section 5).
pub const MAX_PUSH_RECORDS: usize = 10_000;
/// Most JSON bytes of records per `streams.push`, leaving room for the envelope under 1 MiB.
pub const MAX_PUSH_BYTES: usize = 768 * 1024;

/// Exponential back-off, 1 s doubling to 60 s.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    next_ms: u64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next_ms: 1_000 }
    }
}

impl Backoff {
    /// The next wait, then doubles it.
    pub fn next(&mut self) -> Duration {
        let d = Duration::from_millis(self.next_ms);
        self.next_ms = (self.next_ms * 2).min(60_000);
        d
    }

    /// Back to 1 s after a success.
    pub fn reset(&mut self) {
        self.next_ms = 1_000;
    }
}

/// Connects to `ogund` with the bridge's credential.
///
/// # Errors
///
/// The client's error.
pub async fn connect(paths: &OgunPaths, credential: &str) -> Result<Client, ApiError> {
    Client::connect(
        paths,
        credential,
        ClientInfo {
            name: "ogun-ingest".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await
}

/// How much of a push the bridge may delete from its log: records up to `accepted_through`, but
/// only when the answer cannot mean "nothing accepted". Resending is always safe, so an ambiguous
/// answer deletes nothing.
pub fn trim_through(first_seq: u64, last_seq: u64, result: &PushResult) -> Option<u64> {
    let through = result.accepted_through.min(last_seq);
    let certain = if result.retry_after_ms == 0 {
        result.accepted_through >= first_seq
    } else {
        result.accepted_through > first_seq
    };
    certain.then_some(through)
}

/// Runs the forwarder of one source until `drain` is set and the log is empty (or the daemon
/// says the stream is gone).
pub async fn run(
    spec: Arc<SourceSpec>,
    log: LogHandle,
    counters: Arc<Counters>,
    paths: OgunPaths,
    credential: String,
    mut drain: watch::Receiver<bool>,
) {
    let mut client: Option<Client> = None;
    let mut backoff = Backoff::default();
    loop {
        let draining = *drain.borrow();
        if client.is_none() {
            match connect(&paths, &credential).await {
                Ok(c) => {
                    tracing::info!(source = %spec.id(), "forwarder connected to ogund");
                    backoff.reset();
                    client = Some(c);
                }
                Err(e) => {
                    counters.set_error(format!("ogund: {}", e.code()));
                    let wait = backoff.next();
                    tracing::warn!(source = %spec.id(), code = e.code(), wait_ms = wait.as_millis() as u64, "cannot reach ogund; retrying");
                    tokio::time::sleep(wait).await;
                    continue;
                }
            }
        }
        let batch = match log.read(0, MAX_PUSH_RECORDS).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(source = %spec.id(), error = %e, "cannot read the ingest log");
                tokio::time::sleep(backoff.next()).await;
                continue;
            }
        };
        if batch.is_empty() {
            if draining {
                return;
            }
            tokio::select! {
                () = log.written() => {}
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = drain.changed() => {}
            }
            continue;
        }
        let first_seq = batch[0].seq;
        let mut records = Vec::new();
        let mut bytes = 0usize;
        let mut last_seq = first_seq;
        for (i, logged) in batch.iter().enumerate() {
            if logged.seq != first_seq + i as u64 {
                break;
            }
            let Ok(r) = to_record(logged, &spec.window, &[]) else {
                break;
            };
            let record = StreamRecord {
                event_time_ms: r.event_ms,
                key: r.key,
                payload: r.payload,
            };
            let size = serde_json::to_vec(&record).map_or(usize::MAX, |b| b.len());
            if !records.is_empty() && bytes + size > MAX_PUSH_BYTES {
                break;
            }
            bytes += size;
            last_seq = logged.seq;
            records.push(record);
        }
        if records.is_empty() {
            // The oldest record no longer fits the window settings (a new workflow version
            // changed the time or key field). It can never be pushed: count it and let it go.
            Counters::add(&counters.invalid, 1);
            tracing::warn!(source = %spec.id(), seq = first_seq, "logged record no longer has a usable time or key field; released");
            let _ = log.release(first_seq).await;
            continue;
        }
        let params = PushParams {
            workflow: spec.workflow.clone(),
            node: spec.node.clone(),
            first_seq,
            records,
        };
        let Some(c) = client.as_mut() else {
            continue;
        };
        match c.call::<_, PushResult>(name::STREAMS_PUSH, &params).await {
            Ok(result) => {
                backoff.reset();
                if let Some(through) = trim_through(first_seq, last_seq, &result) {
                    match log.release(through).await {
                        Ok(n) => Counters::add(&counters.forwarded, n),
                        Err(e) => {
                            tracing::error!(source = %spec.id(), error = %e, "cannot trim the ingest log")
                        }
                    }
                } else if result.retry_after_ms == 0 {
                    // The daemon accepted nothing and gave no pause: do not spin.
                    tokio::time::sleep(backoff.next()).await;
                }
                Counters::add(&counters.daemon_dropped, result.dropped);
                Counters::add(&counters.daemon_late, result.late);
                if result.retry_after_ms > 0 {
                    tracing::info!(source = %spec.id(), retry_after_ms = result.retry_after_ms, "ogund's stream buffer is full; pausing");
                    tokio::time::sleep(Duration::from_millis(result.retry_after_ms.min(300_000)))
                        .await;
                }
            }
            Err(e) => {
                Counters::add(&counters.push_errors, 1);
                counters.set_error(format!("streams.push: {}", e.code()));
                let wait = match e.code() {
                    "api.unavailable" => {
                        client = None;
                        backoff.next()
                    }
                    "stream.full" => retry_after(&e).unwrap_or_else(|| backoff.next()),
                    "stream.unknown" | "workflow.not_found" if draining => {
                        tracing::warn!(source = %spec.id(), "stream is gone; records left in the bridge log");
                        return;
                    }
                    "api.payload_too_large" => {
                        // Should not happen with the byte cap; push smaller batches by
                        // releasing nothing and retrying after a pause.
                        Duration::from_secs(30)
                    }
                    _ => backoff.next(),
                };
                tracing::warn!(source = %spec.id(), code = e.code(), wait_ms = wait.as_millis() as u64, "streams.push failed");
                tokio::time::sleep(wait).await;
            }
        }
    }
}

fn retry_after(e: &ApiError) -> Option<Duration> {
    e.data
        .details
        .as_ref()
        .and_then(|d| d.get("retry_after_ms"))
        .and_then(serde_json::Value::as_u64)
        .map(|ms| Duration::from_millis(ms.min(300_000)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(accepted_through: u64, retry_after_ms: u64) -> PushResult {
        PushResult {
            accepted_through,
            dropped: 0,
            late: 0,
            retry_after_ms,
        }
    }

    #[test]
    fn trimming_never_guesses() {
        assert_eq!(trim_through(0, 9, &result(9, 0)), Some(9));
        assert_eq!(trim_through(0, 0, &result(0, 0)), Some(0));
        assert_eq!(trim_through(10, 19, &result(14, 500)), Some(14));
        // Full on the very first record: 0 could mean "seq 0 accepted" or "none".
        assert_eq!(trim_through(0, 9, &result(0, 500)), None);
        // An older frontier than this batch: nothing of it is accepted.
        assert_eq!(trim_through(10, 19, &result(5, 500)), None);
        // Never past what was sent.
        assert_eq!(trim_through(0, 9, &result(50, 0)), Some(9));
    }

    #[test]
    fn backoff_doubles_to_a_minute() {
        let mut b = Backoff::default();
        let waits: Vec<u64> = (0..8).map(|_| b.next().as_secs()).collect();
        assert_eq!(waits, vec![1, 2, 4, 8, 16, 32, 60, 60]);
        b.reset();
        assert_eq!(b.next().as_secs(), 1);
    }
}
