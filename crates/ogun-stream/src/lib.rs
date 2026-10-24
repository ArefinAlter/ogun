//! Continuous streams as windowed partitions (Ogun spec O-09; ADR 0007).
//!
//! Ogun does not embed a stream engine. A stream source writes records to a durable ingest log;
//! records are assigned to tumbling event-time windows; when the watermark passes a window's end,
//! the window is **sealed** into a deterministic, content-addressed batch that becomes an ordinary
//! partition for the batch nodes downstream. Late records within the allowed lateness produce a
//! new **revision** of the window; after that the window is **final** and later records go to an
//! explicit `late` output, never silently dropped.
//!
//! Exactly-once is stated honestly: it is effectively-once by content digest. Re-sealing the same
//! records yields the same BLAKE3 digest, which the windower recognises as the same revision.

#![forbid(unsafe_code)]

pub mod seal;
pub mod state;
pub mod window;

pub use seal::{SealedBatch, seal, window_key};
pub use state::{
    WINDOW_TRANSITIONS, WindowCause, WindowState, WindowTransition, window_transition,
};
pub use window::{Action, Counters, Ingested, Record, SpecError, WindowSpec, Windower};
