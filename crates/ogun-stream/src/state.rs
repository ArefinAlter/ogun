//! The window state machine (O-09 section 5), table-driven like O-01.

use serde::{Deserialize, Serialize};

/// The state of one event-time window of one stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowState {
    /// Accepting records; the watermark has not passed the window's end.
    Open,
    /// Being materialized into an artifact. Idempotent: a crash here re-seals the same bytes.
    Sealing,
    /// Committed as revision `n`; late records within the allowed lateness may still arrive.
    Sealed,
    /// A late record arrived after revision `n`; a new revision is due after the debounce.
    Dirty,
    /// Past `end + allowed_lateness`; immutable. Later records go to the `late` output.
    Final,
}

/// Why a window changes state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowCause {
    /// The watermark passed the window's end.
    WatermarkPassed,
    /// The sealed artifact and the stream cursor committed in one run-store transaction.
    SealCommitted,
    /// A record within the allowed lateness arrived for a sealed window.
    LateRecord,
    /// The re-seal debounce elapsed.
    ResealDue,
    /// `end + allowed_lateness` passed.
    LatenessExpired,
}

/// One row of the window transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowTransition {
    /// Source state.
    pub from: WindowState,
    /// Cause.
    pub cause: WindowCause,
    /// Target state.
    pub to: WindowState,
}

const fn t(from: WindowState, cause: WindowCause, to: WindowState) -> WindowTransition {
    WindowTransition { from, cause, to }
}

use WindowCause as C;
use WindowState as W;

/// The complete window transition table (O-09 section 5.2).
pub const WINDOW_TRANSITIONS: &[WindowTransition] = &[
    t(W::Open, C::WatermarkPassed, W::Sealing),
    t(W::Open, C::LatenessExpired, W::Sealing),
    t(W::Sealing, C::SealCommitted, W::Sealed),
    t(W::Sealed, C::LateRecord, W::Dirty),
    t(W::Sealed, C::LatenessExpired, W::Final),
    t(W::Dirty, C::ResealDue, W::Sealing),
    t(W::Dirty, C::LatenessExpired, W::Sealing),
];

/// The target state, or `None` when the table has no such row.
#[must_use]
pub fn window_transition(from: WindowState, cause: WindowCause) -> Option<WindowState> {
    WINDOW_TRANSITIONS
        .iter()
        .find(|r| r.from == from && r.cause == cause)
        .map(|r| r.to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn deterministic_and_final_is_terminal() {
        let mut seen = HashSet::new();
        for r in WINDOW_TRANSITIONS {
            assert!(seen.insert((r.from, r.cause)));
            assert_ne!(r.from, W::Final);
        }
    }

    #[test]
    fn every_path_ends_final() {
        // From any non-final state, LatenessExpired (possibly after one seal) reaches Final.
        for s in [W::Open, W::Sealing, W::Sealed, W::Dirty] {
            let mut state = s;
            for _ in 0..4 {
                state = window_transition(state, C::LatenessExpired)
                    .or_else(|| window_transition(state, C::SealCommitted))
                    .unwrap_or(state);
            }
            assert_eq!(state, W::Final, "{s:?}");
        }
    }
}
