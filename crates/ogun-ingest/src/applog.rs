//! One durable ingest log per source, owned by one thread (O-10 section 3).
//!
//! Every adapter appends here first and acknowledges its source only after the append commits.
//! The thread does group commit: it gathers appends for up to `batch_max_delay_ms` or
//! `batch_max_records`, writes them in one `ogun_edge::IngestLog` transaction with immediate
//! durability, and then answers each caller. Reads and releases from the forwarder go through the
//! same thread, so it is the log's only writer and the outcome of a `drop_newest` batch can be
//! computed exactly.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ogun_edge::{Incoming, IngestLog, LogError, LogStats, Logged};
use ogun_ir::model::{StreamBuffer, WhenFull};
use tokio::sync::{Notify, oneshot};

use crate::status::Counters;

/// How a batch went for one caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// The commit returned. `written` records are durable; `dropped` were refused by
    /// `drop_newest` and are final (counted, not retried).
    Committed {
        /// Records written.
        written: u64,
        /// Records refused by `drop_newest`.
        dropped: u64,
    },
    /// Nothing was written: the log is full under `block`, or the disk failed (treated as `block`,
    /// O-10 section 3.3). Do not acknowledge; retry later.
    Refused,
}

enum Cmd {
    Append(Vec<Incoming>, oneshot::Sender<Reply>),
    Read(u64, usize, oneshot::Sender<Result<Vec<Logged>, String>>),
    Release(u64, oneshot::Sender<Result<u64, String>>),
    Stats(oneshot::Sender<Result<(LogStats, u64), String>>),
    Shutdown,
}

/// Group-commit limits.
#[derive(Debug, Clone, Copy)]
pub struct BatchPolicy {
    /// Most records per transaction.
    pub max_records: usize,
    /// Longest wait between the first record of a batch and its commit.
    pub max_delay: Duration,
}

/// A handle to a source's log thread. Cheap to clone.
#[derive(Debug, Clone)]
pub struct LogHandle {
    tx: mpsc::Sender<Cmd>,
    notify: Arc<Notify>,
}

impl LogHandle {
    /// Opens the log at `path` and starts its thread. Blocking; call from `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// The log's error (for example [`LogError::AlreadyOpen`]).
    pub fn open(
        path: &Path,
        buffer: &StreamBuffer,
        policy: BatchPolicy,
        counters: Arc<Counters>,
    ) -> Result<(Self, std::thread::JoinHandle<()>), LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LogError::Storage(e.kind().to_string()))?;
        }
        let log = IngestLog::open(path, buffer.max_buffer_bytes, buffer.when_full)?;
        let (tx, rx) = mpsc::channel();
        let notify = Arc::new(Notify::new());
        let worker = Worker {
            log,
            max_bytes: buffer.max_buffer_bytes,
            when_full: buffer.when_full,
            policy,
            notify: notify.clone(),
            counters,
        };
        let thread = std::thread::Builder::new()
            .name("ogun-ingest-log".into())
            .spawn(move || worker.run(&rx))
            .map_err(|e| LogError::Storage(e.kind().to_string()))?;
        Ok((Self { tx, notify }, thread))
    }

    /// Queues records for the next group commit and returns the receiver of its outcome.
    /// Submissions are committed in the order they are made.
    pub fn submit(&self, records: Vec<Incoming>) -> oneshot::Receiver<Reply> {
        let (tx, rx) = oneshot::channel();
        if records.is_empty() {
            let _ = tx.send(Reply::Committed {
                written: 0,
                dropped: 0,
            });
        } else if let Err(mpsc::SendError(Cmd::Append(_, tx))) =
            self.tx.send(Cmd::Append(records, tx))
        {
            let _ = tx.send(Reply::Refused);
        }
        rx
    }

    /// Appends records and waits for the group commit.
    pub async fn append(&self, records: Vec<Incoming>) -> Reply {
        self.submit(records).await.unwrap_or(Reply::Refused)
    }

    /// Records with `seq >= from`, at most `limit`.
    pub async fn read(&self, from: u64, limit: usize) -> Result<Vec<Logged>, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Read(from, limit, tx))
            .map_err(|_| "log closed".to_owned())?;
        rx.await.map_err(|_| "log closed".to_owned())?
    }

    /// Deletes records with `seq <= through` (the daemon holds them now).
    pub async fn release(&self, through: u64) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Release(through, tx))
            .map_err(|_| "log closed".to_owned())?;
        rx.await.map_err(|_| "log closed".to_owned())?
    }

    /// The log's counters and its record count.
    pub async fn stats(&self) -> Result<(LogStats, u64), String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Stats(tx))
            .map_err(|_| "log closed".to_owned())?;
        rx.await.map_err(|_| "log closed".to_owned())?
    }

    /// Resolves after a commit that wrote records.
    pub async fn written(&self) {
        self.notify.notified().await;
    }

    /// Commits what is pending and stops the thread; the file is closed when it exits.
    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

struct Worker {
    log: IngestLog,
    max_bytes: u64,
    when_full: WhenFull,
    policy: BatchPolicy,
    notify: Arc<Notify>,
    counters: Arc<Counters>,
}

type Pending = Vec<(Vec<Incoming>, oneshot::Sender<Reply>)>;

impl Worker {
    fn run(self, rx: &mpsc::Receiver<Cmd>) {
        let mut pending: Pending = Vec::new();
        let mut count = 0usize;
        let mut deadline: Option<Instant> = None;
        loop {
            let cmd = match deadline {
                None => rx.recv().ok(),
                Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        self.commit(std::mem::take(&mut pending));
                        count = 0;
                        deadline = None;
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                },
            };
            match cmd {
                Some(Cmd::Append(records, reply)) => {
                    count += records.len();
                    pending.push((records, reply));
                    if count >= self.policy.max_records {
                        self.commit(std::mem::take(&mut pending));
                        count = 0;
                        deadline = None;
                    } else if deadline.is_none() {
                        deadline = Some(Instant::now() + self.policy.max_delay);
                    }
                }
                Some(Cmd::Read(from, limit, reply)) => {
                    let _ = reply.send(self.log.read(from, limit).map_err(|e| e.to_string()));
                }
                Some(Cmd::Release(through, reply)) => {
                    let _ =
                        reply.send(self.log.release_through(through).map_err(|e| e.to_string()));
                }
                Some(Cmd::Stats(reply)) => {
                    let r = self
                        .log
                        .stats()
                        .and_then(|s| self.log.len().map(|n| (s, n)))
                        .map_err(|e| e.to_string());
                    let _ = reply.send(r);
                }
                Some(Cmd::Shutdown) | None => {
                    self.commit(pending);
                    return;
                }
            }
        }
    }

    /// Which records of the batch `drop_newest` will write, replaying the log's own rule: a record
    /// is written when it still fits, in order.
    fn drop_newest_mask(&self, sizes: &[u64]) -> Option<Vec<bool>> {
        let mut used = self.log.stats().ok()?.bytes_used;
        Some(
            sizes
                .iter()
                .map(|&size| {
                    let fits = used.saturating_add(size) <= self.max_bytes;
                    if fits {
                        used += size;
                    }
                    fits
                })
                .collect(),
        )
    }

    fn commit(&self, pending: Pending) {
        if pending.is_empty() {
            return;
        }
        let all: Vec<Incoming> = pending
            .iter()
            .flat_map(|(r, _)| r.iter().cloned())
            .collect();
        let total = all.len() as u64;
        let mask = if self.when_full == WhenFull::DropNewest {
            let sizes: Vec<u64> = all
                .iter()
                .map(|r| serde_json::to_vec(r).map_or(u64::MAX, |b| b.len() as u64))
                .collect();
            self.drop_newest_mask(&sizes)
        } else {
            None
        };
        match self.log.append(all) {
            Ok(appended) => {
                let mask = match mask {
                    Some(m) if m.iter().filter(|w| **w).count() as u64 == appended.written => m,
                    Some(_) => {
                        // Cannot happen while this thread is the only writer; refuse rather than
                        // acknowledge records that may not be durable. Redelivery is deduplicated.
                        tracing::error!(
                            "drop_newest outcome differs from the log; not acknowledging"
                        );
                        for (_, reply) in pending {
                            let _ = reply.send(Reply::Refused);
                        }
                        return;
                    }
                    None => vec![true; usize::try_from(total).unwrap_or(0)],
                };
                Counters::add(&self.counters.written, appended.written);
                Counters::add(&self.counters.dropped_newest, appended.dropped_newest);
                if appended.dropped_newest > 0 {
                    tracing::warn!(
                        dropped = appended.dropped_newest,
                        "ingest log full: records refused by drop_newest"
                    );
                }
                if appended.evicted_oldest > 0 {
                    tracing::warn!(
                        evicted = appended.evicted_oldest,
                        "ingest log full: oldest records evicted by drop_oldest"
                    );
                }
                let mut i = 0;
                for (records, reply) in pending {
                    let n = records.len();
                    let written = mask[i..i + n].iter().filter(|w| **w).count() as u64;
                    i += n;
                    let _ = reply.send(Reply::Committed {
                        written,
                        dropped: n as u64 - written,
                    });
                }
                if appended.written > 0 {
                    self.notify.notify_one();
                }
            }
            Err(e) => {
                Counters::add(&self.counters.refused, total);
                match e {
                    LogError::Full { used, max } => {
                        tracing::warn!(used, max, "ingest log full under block; not acknowledging");
                    }
                    other => {
                        tracing::error!(error = %other, "ingest log write failed; treated as full");
                        self.counters.set_error(format!("ingest log: {other}"));
                    }
                }
                for (_, reply) in pending {
                    let _ = reply.send(Reply::Refused);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(i: u64) -> Incoming {
        Incoming {
            received_ms: 0,
            origin: "t".into(),
            payload: json!({"device_id": "d", "seq": i, "ts_ms": 0}),
        }
    }

    fn open(dir: &Path, max: u64, when_full: WhenFull) -> LogHandle {
        let buffer = StreamBuffer {
            max_buffer_bytes: max,
            when_full,
            dedupe_key: vec![],
        };
        LogHandle::open(
            &dir.join("log.redb"),
            &buffer,
            BatchPolicy {
                max_records: 1000,
                max_delay: Duration::from_millis(20),
            },
            Arc::new(Counters::default()),
        )
        .unwrap()
        .0
    }

    #[tokio::test]
    async fn concurrent_appends_share_a_commit_and_get_their_own_counts() {
        let dir = tempfile::tempdir().unwrap();
        let log = open(dir.path(), 1 << 20, WhenFull::Block);
        let (a, b) = tokio::join!(log.append(vec![rec(0), rec(1)]), log.append(vec![rec(2)]));
        assert_eq!(
            a,
            Reply::Committed {
                written: 2,
                dropped: 0
            }
        );
        assert_eq!(
            b,
            Reply::Committed {
                written: 1,
                dropped: 0
            }
        );
        let read = log.read(0, 10).await.unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(log.release(1).await.unwrap(), 2);
        assert_eq!(log.stats().await.unwrap().1, 1);
    }

    #[tokio::test]
    async fn drop_newest_reports_exactly_what_was_refused() {
        let dir = tempfile::tempdir().unwrap();
        let one = serde_json::to_vec(&rec(0)).unwrap().len() as u64;
        let log = open(dir.path(), one * 2, WhenFull::DropNewest);
        let r = log.append(vec![rec(0), rec(1), rec(2)]).await;
        assert_eq!(
            r,
            Reply::Committed {
                written: 2,
                dropped: 1
            }
        );
        let blocked = open(&dir.path().join("b"), one * 2, WhenFull::Block);
        assert_eq!(
            blocked.append(vec![rec(0), rec(1), rec(2)]).await,
            Reply::Refused
        );
    }
}
