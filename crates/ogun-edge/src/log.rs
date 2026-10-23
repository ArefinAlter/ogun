//! The durable ingest log (O-10 section 3).
//!
//! Records from a stream source are appended in batches, one write transaction per batch with
//! immediate durability (group commit), and the source is acknowledged only after the batch
//! commits. That turns MQTT, HTTP push and BLE, which cannot replay, into a replayable log.
//!
//! Every record gets an ingest sequence number, unique and increasing per log. Records become
//! evictable once the windows they belong to are final and their artifacts are safe (sealed, and
//! synced to the hub when there is one); the caller says so with [`IngestLog::release_through`].

use std::path::Path;

use ogun_ir::model::WhenFull;
use redb::{Durability, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const RECORDS: TableDefinition<'static, u64, &'static [u8]> = TableDefinition::new("records");
const META: TableDefinition<'static, &'static str, &'static [u8]> = TableDefinition::new("meta");

/// Log failures.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The log is full and its policy is `block`: stop acknowledging the source.
    #[error("ingest log full ({used} of {max} bytes); not acknowledging")]
    Full {
        /// Bytes in use.
        used: u64,
        /// The limit.
        max: u64,
    },
    /// Another process holds the log.
    #[error("the ingest log is already open in another process")]
    AlreadyOpen,
    /// The database failed.
    #[error("storage: {0}")]
    Storage(String),
    /// A row could not be decoded.
    #[error("corrupt ingest log row")]
    Corrupt,
}

fn storage(e: impl std::fmt::Display) -> LogError {
    LogError::Storage(e.to_string())
}

/// One record as the protocol adapter received it, before sequencing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incoming {
    /// Ingest time, Unix ms, from the bridge's clock.
    pub received_ms: i64,
    /// Where it came from, for example the MQTT topic. Never a URL with credentials.
    pub origin: String,
    /// The parsed payload.
    pub payload: Value,
}

/// A record with its sequence number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Logged {
    /// Ingest sequence number.
    pub seq: u64,
    /// The record.
    #[serde(flatten)]
    pub record: Incoming,
}

/// What an append did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Appended {
    /// Sequence of the first record written, if any.
    pub first_seq: Option<u64>,
    /// Records written; acknowledge exactly these to the source.
    pub written: u64,
    /// Records refused under `drop_newest`.
    pub dropped_newest: u64,
    /// Older records evicted under `drop_oldest` to make room.
    pub evicted_oldest: u64,
}

/// Loss and size counters, kept in the log and emitted as health events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogStats {
    /// Next sequence number to assign.
    pub next_seq: u64,
    /// Bytes of record payloads currently stored.
    pub bytes_used: u64,
    /// Every sequence number at or below this has been released (safe to evict).
    pub released_through: Option<u64>,
    /// Records refused under `drop_newest`, all time.
    pub dropped_newest: u64,
    /// Unreleased records evicted under `drop_oldest`, all time. These are data loss.
    pub lost_oldest: u64,
    /// Released records evicted to make room, all time. Not a loss.
    pub evicted_released: u64,
}

/// The ingest log of one stream source.
#[derive(Debug)]
pub struct IngestLog {
    db: redb::Database,
    max_bytes: u64,
    when_full: WhenFull,
}

fn stats(t: &impl ReadableTable<&'static str, &'static [u8]>) -> Result<LogStats, LogError> {
    match t.get("stats").map_err(storage)? {
        None => Ok(LogStats::default()),
        Some(v) => serde_json::from_slice(v.value()).map_err(|_| LogError::Corrupt),
    }
}

impl IngestLog {
    /// Opens or creates the log at `path`.
    ///
    /// # Errors
    ///
    /// [`LogError::AlreadyOpen`] or a storage error.
    pub fn open(path: &Path, max_bytes: u64, when_full: WhenFull) -> Result<Self, LogError> {
        let db = redb::Database::create(path).map_err(|e| match e {
            redb::DatabaseError::DatabaseAlreadyOpen => LogError::AlreadyOpen,
            other => storage(other),
        })?;
        let log = Self {
            db,
            max_bytes,
            when_full,
        };
        let txn = log.write()?;
        txn.open_table(RECORDS).map_err(storage)?;
        txn.open_table(META).map_err(storage)?;
        txn.commit().map_err(storage)?;
        Ok(log)
    }

    fn write(&self) -> Result<redb::WriteTransaction, LogError> {
        let mut txn = self.db.begin_write().map_err(storage)?;
        txn.set_durability(Durability::Immediate).map_err(storage)?;
        Ok(txn)
    }

    /// The counters.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn stats(&self) -> Result<LogStats, LogError> {
        let txn = self.db.begin_read().map_err(storage)?;
        stats(&txn.open_table(META).map_err(storage)?)
    }

    /// Appends a batch in one durable transaction and returns what to acknowledge.
    ///
    /// Under `block`, a batch that does not fit is refused whole ([`LogError::Full`]) and nothing
    /// is written; the adapter stops acknowledging and the source queues or retries. Under
    /// `drop_newest`, records that do not fit are refused and counted. Under `drop_oldest`,
    /// released records are evicted first, then unreleased ones, which counts as loss.
    ///
    /// # Errors
    ///
    /// [`LogError::Full`] under `block`, or a storage error (nothing is written then).
    pub fn append(&self, batch: Vec<Incoming>) -> Result<Appended, LogError> {
        let encoded: Vec<Vec<u8>> = batch
            .iter()
            .map(|r| serde_json::to_vec(r).map_err(storage))
            .collect::<Result<_, _>>()?;
        let txn = self.write()?;
        let mut out = Appended::default();
        {
            let mut meta = txn.open_table(META).map_err(storage)?;
            let mut records = txn.open_table(RECORDS).map_err(storage)?;
            let mut st = stats(&meta)?;
            let incoming: u64 = encoded.iter().map(|b| b.len() as u64).sum();
            if st.bytes_used.saturating_add(incoming) > self.max_bytes
                && self.when_full == WhenFull::Block
            {
                return Err(LogError::Full {
                    used: st.bytes_used,
                    max: self.max_bytes,
                });
            }
            for bytes in &encoded {
                let size = bytes.len() as u64;
                while st.bytes_used.saturating_add(size) > self.max_bytes
                    && self.when_full == WhenFull::DropOldest
                {
                    let Some(oldest) = records
                        .first()
                        .map_err(storage)?
                        .map(|(k, v)| (k.value(), v.value().len() as u64))
                    else {
                        break;
                    };
                    records.remove(oldest.0).map_err(storage)?;
                    st.bytes_used = st.bytes_used.saturating_sub(oldest.1);
                    out.evicted_oldest += 1;
                    if st.released_through.is_some_and(|r| oldest.0 <= r) {
                        st.evicted_released += 1;
                    } else {
                        st.lost_oldest += 1;
                    }
                }
                if st.bytes_used.saturating_add(size) > self.max_bytes {
                    out.dropped_newest += 1;
                    st.dropped_newest += 1;
                    continue;
                }
                let seq = st.next_seq;
                st.next_seq += 1;
                records.insert(seq, bytes.as_slice()).map_err(storage)?;
                st.bytes_used += size;
                out.first_seq.get_or_insert(seq);
                out.written += 1;
            }
            let st_bytes = serde_json::to_vec(&st).map_err(storage)?;
            meta.insert("stats", st_bytes.as_slice()).map_err(storage)?;
        }
        txn.commit().map_err(storage)?;
        Ok(out)
    }

    /// Records with `seq >= from`, in order, at most `limit`.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn read(&self, from: u64, limit: usize) -> Result<Vec<Logged>, LogError> {
        let txn = self.db.begin_read().map_err(storage)?;
        let t = txn.open_table(RECORDS).map_err(storage)?;
        let mut out = Vec::new();
        for row in t.range(from..).map_err(storage)?.take(limit) {
            let (k, v) = row.map_err(storage)?;
            let record: Incoming =
                serde_json::from_slice(v.value()).map_err(|_| LogError::Corrupt)?;
            out.push(Logged {
                seq: k.value(),
                record,
            });
        }
        Ok(out)
    }

    /// Declares every record with `seq <= through` safe to evict (its windows are final and their
    /// artifacts sealed, and synced when a hub is configured), and deletes them now.
    /// Returns how many were deleted.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn release_through(&self, through: u64) -> Result<u64, LogError> {
        let txn = self.write()?;
        let mut deleted = 0;
        {
            let mut meta = txn.open_table(META).map_err(storage)?;
            let mut records = txn.open_table(RECORDS).map_err(storage)?;
            let mut st = stats(&meta)?;
            let mut keys = Vec::new();
            for row in records.range(..=through).map_err(storage)? {
                let (k, v) = row.map_err(storage)?;
                keys.push((k.value(), v.value().len() as u64));
            }
            for (k, size) in keys {
                records.remove(k).map_err(storage)?;
                st.bytes_used = st.bytes_used.saturating_sub(size);
                deleted += 1;
            }
            st.released_through = Some(st.released_through.map_or(through, |r| r.max(through)));
            let st_bytes = serde_json::to_vec(&st).map_err(storage)?;
            meta.insert("stats", st_bytes.as_slice()).map_err(storage)?;
        }
        txn.commit().map_err(storage)?;
        Ok(deleted)
    }

    /// Records currently stored.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn len(&self) -> Result<u64, LogError> {
        let txn = self.db.begin_read().map_err(storage)?;
        txn.open_table(RECORDS)
            .map_err(storage)?
            .len()
            .map_err(storage)
    }

    /// True when no records are stored.
    ///
    /// # Errors
    ///
    /// A storage error.
    pub fn is_empty(&self) -> Result<bool, LogError> {
        self.len().map(|n| n == 0)
    }
}
