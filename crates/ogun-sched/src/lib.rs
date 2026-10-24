//! Ogun scheduling (Ogun spec O-04): when runs start, which partitions they cover, and how much
//! they may spend.
//!
//! Everything here is pure: time comes in as Unix milliseconds, so tests and replays are
//! deterministic and the daemon owns the clock.

#![forbid(unsafe_code)]

pub mod backfill;
pub mod budget;
pub mod civil;
pub mod cron;
pub mod partitions;
pub mod trigger;

pub use backfill::{BackfillPlan, lanes, plan};
pub use budget::{Ledger, OverBudget, ReservationId, Resource};
pub use cron::{Cron, CronError};
pub use partitions::{PartitionError, Window, complete, range, window};
pub use trigger::next_fire;
