//! The windower: assigns records to event-time windows, tracks per-key watermarks, and says when
//! a window must be sealed, re-sealed or finalized (O-09 sections 3 to 6).
//!
//! It is deterministic: time comes in as Unix milliseconds and nothing reads a clock. Its whole
//! state is serialisable, so the daemon commits it with each seal as the stream checkpoint.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{WindowCause, WindowState, window_transition};

const DAY_MS: u64 = 86_400_000;

/// Window and watermark settings (the `partitions` member of a stream workflow, O-09 section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSpec {
    /// Tumbling window size. Divides a day exactly; at least 1 000.
    pub size_ms: u64,
    /// How long after a window's end late records still revise it. At most 7 days.
    pub allowed_lateness_ms: u64,
    /// Debounce between a late record and the re-seal it causes.
    pub reseal_after_ms: u64,
    /// Bounded out-of-orderness: a key's watermark is its newest event time minus this.
    pub max_delay_ms: u64,
    /// A key silent for this long stops holding the watermark back.
    pub idle_timeout_ms: u64,
    /// Event times further than this in the future (relative to ingest time) are clamped.
    pub max_future_ms: u64,
}

/// Why a [`WindowSpec`] is not acceptable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    /// `size_ms` is below 1 000 or does not divide a day.
    #[error("size_ms must be at least 1000 and divide 86400000")]
    Size,
    /// `allowed_lateness_ms` exceeds 7 days.
    #[error("allowed_lateness_ms is at most 7 days")]
    Lateness,
}

impl WindowSpec {
    /// Checks the limits of O-09 section 2.
    ///
    /// # Errors
    ///
    /// [`SpecError`] naming the first violated limit.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.size_ms < 1_000 || !DAY_MS.is_multiple_of(self.size_ms) {
            return Err(SpecError::Size);
        }
        if self.allowed_lateness_ms > 7 * DAY_MS {
            return Err(SpecError::Lateness);
        }
        Ok(())
    }

    fn start_of(&self, event_ms: i64) -> i64 {
        let size = i64::try_from(self.size_ms).unwrap_or(i64::MAX);
        event_ms.div_euclid(size) * size
    }

    fn end_of(&self, start: i64) -> i64 {
        start.saturating_add(i64::try_from(self.size_ms).unwrap_or(i64::MAX))
    }

    fn ms(v: u64) -> i64 {
        i64::try_from(v).unwrap_or(i64::MAX)
    }
}

/// One record from a stream source, after parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The watermark key, usually the device id.
    pub key: String,
    /// Event time from the declared time field, Unix milliseconds.
    pub event_ms: i64,
    /// Ogun's ingest sequence number: unique and increasing per stream.
    pub seq: u64,
    /// The idempotency key (for example `device_id/device_seq`); duplicates are dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe: Option<String>,
    /// The parsed payload. Data, not IR: floats are allowed here.
    pub payload: Value,
    /// True when the event time was clamped (O-09 section 3.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clamped: bool,
}

/// What happened to an ingested record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Ingested {
    /// Added to the window starting at `window_start`.
    Accepted {
        /// Window start, Unix ms.
        window_start: i64,
    },
    /// Added to a sealed window, which is now dirty.
    Revises {
        /// Window start, Unix ms.
        window_start: i64,
    },
    /// Same dedupe key seen in the same window; dropped.
    Duplicate,
    /// The window is final; the record went to the `late` output.
    Late,
}

/// Something the daemon must do, from [`Windower::poll`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum Action {
    /// Materialize `revision` of the window from these records, then call
    /// [`Windower::seal_committed`] in the same transaction that stores the artifact.
    Seal {
        /// Window start, Unix ms.
        window_start: i64,
        /// Revision to produce, from 1.
        revision: u32,
        /// The records, sorted by `(key, event_ms, seq)`.
        records: Vec<Record>,
    },
    /// The window is final; its last revision stands.
    Finalized {
        /// Window start, Unix ms.
        window_start: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Window {
    state: WindowState,
    records: Vec<Record>,
    dedupe: BTreeSet<String>,
    revision: u32,
    digest: Option<String>,
    dirty_since: Option<i64>,
}

/// Per-stream windowing state. Serialise it as the stream checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Windower {
    spec: WindowSpec,
    windows: BTreeMap<i64, Window>,
    /// Newest event time and last ingest time per key.
    keys: BTreeMap<String, (i64, i64)>,
    late: Vec<Record>,
    counters: Counters,
}

/// Loss and anomaly counters, emitted as events and kept in the checkpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    /// Records accepted into a window.
    pub accepted: u64,
    /// Duplicates dropped.
    pub duplicates: u64,
    /// Records routed to the `late` output.
    pub late: u64,
    /// Records whose event time was clamped.
    pub clamped: u64,
    /// Re-seals that produced a new revision.
    pub revisions: u64,
}

impl Windower {
    /// A windower with no state.
    ///
    /// # Errors
    ///
    /// [`SpecError`] when the spec is outside O-09's limits.
    pub fn new(spec: WindowSpec) -> Result<Self, SpecError> {
        spec.validate()?;
        Ok(Self {
            spec,
            windows: BTreeMap::new(),
            keys: BTreeMap::new(),
            late: Vec::new(),
            counters: Counters::default(),
        })
    }

    /// The counters.
    #[must_use]
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// The state of the window starting at `start`, if it exists.
    #[must_use]
    pub fn state(&self, start: i64) -> Option<WindowState> {
        self.windows.get(&start).map(|w| w.state)
    }

    /// Takes the records routed to the `late` output since the last call.
    pub fn take_late(&mut self) -> Vec<Record> {
        std::mem::take(&mut self.late)
    }

    /// The stream watermark at `now_ms`: the minimum over active keys of newest event time minus
    /// `max_delay_ms`. Keys idle for `idle_timeout_ms` do not hold it back; with no active key it
    /// advances with ingest time (`now - max_delay`).
    #[must_use]
    pub fn watermark(&self, now_ms: i64) -> i64 {
        let delay = WindowSpec::ms(self.spec.max_delay_ms);
        let idle = WindowSpec::ms(self.spec.idle_timeout_ms);
        self.keys
            .values()
            .filter(|(_, seen)| now_ms.saturating_sub(*seen) < idle)
            .map(|(newest, _)| newest.saturating_sub(delay))
            .min()
            .unwrap_or_else(|| now_ms.saturating_sub(delay))
    }

    /// Ingests one record at ingest time `now_ms`.
    pub fn ingest(&mut self, mut record: Record, now_ms: i64) -> Ingested {
        let horizon = now_ms.saturating_add(WindowSpec::ms(self.spec.max_future_ms));
        if record.event_ms > horizon {
            record.event_ms = now_ms;
            record.clamped = true;
            self.counters.clamped += 1;
        }
        let start = self.spec.start_of(record.event_ms);
        let entry = self
            .keys
            .entry(record.key.clone())
            .or_insert((record.event_ms, now_ms));
        entry.0 = entry.0.max(record.event_ms);
        entry.1 = now_ms;

        let final_at = self
            .spec
            .end_of(start)
            .saturating_add(WindowSpec::ms(self.spec.allowed_lateness_ms));
        // A window that is final, or that would be final the moment it opened, takes no records.
        let is_final = match self.windows.get(&start) {
            Some(w) => w.state == WindowState::Final,
            None => now_ms >= final_at,
        };
        if is_final {
            self.counters.late += 1;
            self.late.push(record);
            return Ingested::Late;
        }
        let window = self.windows.entry(start).or_insert_with(|| Window {
            state: WindowState::Open,
            records: Vec::new(),
            dedupe: BTreeSet::new(),
            revision: 0,
            digest: None,
            dirty_since: None,
        });
        if let Some(key) = &record.dedupe
            && !window.dedupe.insert(key.clone())
        {
            self.counters.duplicates += 1;
            return Ingested::Duplicate;
        }
        window.records.push(record);
        self.counters.accepted += 1;
        match window.state {
            WindowState::Sealed => {
                window.state = window_transition(WindowState::Sealed, WindowCause::LateRecord)
                    .unwrap_or(WindowState::Dirty);
                window.dirty_since = Some(now_ms);
                Ingested::Revises {
                    window_start: start,
                }
            }
            WindowState::Dirty | WindowState::Sealing => Ingested::Revises {
                window_start: start,
            },
            _ => Ingested::Accepted {
                window_start: start,
            },
        }
    }

    /// Advances every window at `now_ms` and returns the seals and finalizations due, oldest first.
    pub fn poll(&mut self, now_ms: i64) -> Vec<Action> {
        let watermark = self.watermark(now_ms);
        let lateness = WindowSpec::ms(self.spec.allowed_lateness_ms);
        let reseal = WindowSpec::ms(self.spec.reseal_after_ms);
        let mut out = Vec::new();
        for (&start, w) in &mut self.windows {
            let end = self.spec.end_of(start);
            let expired = now_ms >= end.saturating_add(lateness) && watermark >= end;
            let cause = match w.state {
                WindowState::Open if watermark >= end => Some(WindowCause::WatermarkPassed),
                WindowState::Open if expired => Some(WindowCause::LatenessExpired),
                WindowState::Dirty if expired => Some(WindowCause::LatenessExpired),
                WindowState::Dirty
                    if w.dirty_since
                        .is_some_and(|t| now_ms.saturating_sub(t) >= reseal) =>
                {
                    Some(WindowCause::ResealDue)
                }
                WindowState::Sealed if expired => Some(WindowCause::LatenessExpired),
                _ => None,
            };
            let Some(cause) = cause else { continue };
            let Some(next) = window_transition(w.state, cause) else {
                continue;
            };
            w.state = next;
            match next {
                WindowState::Sealing => {
                    let mut records = w.records.clone();
                    records.sort_by(|a, b| {
                        (&a.key, a.event_ms, a.seq).cmp(&(&b.key, b.event_ms, b.seq))
                    });
                    out.push(Action::Seal {
                        window_start: start,
                        revision: w.revision + 1,
                        records,
                    });
                }
                WindowState::Final => {
                    w.records.clear();
                    w.dedupe.clear();
                    out.push(Action::Finalized {
                        window_start: start,
                    });
                }
                _ => {}
            }
        }
        out
    }

    /// Records that a seal committed with this artifact digest. Returns `true` when it is a new
    /// revision, `false` when the digest equals the previous revision's (a duplicate late record
    /// or a crash-and-retry), in which case the revision number does not advance.
    pub fn seal_committed(&mut self, window_start: i64, digest: &str) -> bool {
        let Some(w) = self.windows.get_mut(&window_start) else {
            return false;
        };
        let Some(next) = window_transition(w.state, WindowCause::SealCommitted) else {
            return false;
        };
        w.state = next;
        w.dirty_since = None;
        if w.digest.as_deref() == Some(digest) {
            return false;
        }
        w.revision += 1;
        if w.revision > 1 {
            self.counters.revisions += 1;
        }
        w.digest = Some(digest.to_owned());
        true
    }

    /// The current revision and digest of a window.
    #[must_use]
    pub fn revision(&self, window_start: i64) -> Option<(u32, Option<&str>)> {
        self.windows
            .get(&window_start)
            .map(|w| (w.revision, w.digest.as_deref()))
    }

    /// Drops final windows from memory (their artifacts are in the store).
    pub fn compact(&mut self) {
        self.windows.retain(|_, w| w.state != WindowState::Final);
    }
}
