//! Budget reservations (O-04 section 5).
//!
//! Work reserves budget before it starts and commits the actual amount when it ends, so concurrent
//! work cannot overspend together. A reservation that would exceed a limit is refused; the caller
//! moves the node to `waiting_for_user` when its policy is `over_budget`, or fails it otherwise.
//! All amounts are integers (bytes, files, milliseconds, micro-dollars).

use std::collections::BTreeMap;

use ogun_ir::model::Budgets;
use serde::{Deserialize, Serialize};

/// A budgeted quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    /// Bytes acquired or written.
    Bytes,
    /// Files materialized.
    Files,
    /// Wall-clock milliseconds.
    WallMs,
    /// Model spend in micro-dollars.
    LlmUsdMicro,
}

impl Resource {
    fn limit(self, b: &Budgets) -> Option<u64> {
        match self {
            Self::Bytes => b.bytes,
            Self::Files => b.files,
            Self::WallMs => b.wall_ms,
            Self::LlmUsdMicro => b.llm_usd_micro,
        }
    }
}

/// A reservation that does not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{resource:?}: {requested} requested, {available} available")]
pub struct OverBudget {
    /// The quantity.
    pub resource: Resource,
    /// The amount asked for.
    pub requested: u64,
    /// What is left after committed use and open reservations.
    pub available: u64,
}

/// Handle for an open reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReservationId(u64);

/// One budget scope (a run partition or a node), with committed use and open reservations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    limits: Budgets,
    used: BTreeMap<Resource, u64>,
    open: BTreeMap<ReservationId, (Resource, u64)>,
    next: u64,
}

impl Ledger {
    /// A ledger with these limits.
    #[must_use]
    pub fn new(limits: Budgets) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    fn reserved(&self, r: Resource) -> u64 {
        self.open
            .values()
            .filter(|(res, _)| *res == r)
            .map(|(_, n)| n)
            .sum()
    }

    /// Committed use of a resource.
    #[must_use]
    pub fn used(&self, r: Resource) -> u64 {
        self.used.get(&r).copied().unwrap_or(0)
    }

    /// What can still be reserved; `None` when unlimited.
    #[must_use]
    pub fn available(&self, r: Resource) -> Option<u64> {
        r.limit(&self.limits).map(|limit| {
            limit
                .saturating_sub(self.used(r))
                .saturating_sub(self.reserved(r))
        })
    }

    /// Reserves `amount` of `r`.
    ///
    /// # Errors
    ///
    /// [`OverBudget`] when the reservation does not fit; nothing is reserved then.
    pub fn reserve(&mut self, r: Resource, amount: u64) -> Result<ReservationId, OverBudget> {
        if let Some(available) = self.available(r)
            && amount > available
        {
            return Err(OverBudget {
                resource: r,
                requested: amount,
                available,
            });
        }
        let id = ReservationId(self.next);
        self.next += 1;
        self.open.insert(id, (r, amount));
        Ok(id)
    }

    /// Closes a reservation with the amount actually used, which may exceed the reservation (a
    /// download larger than declared). Returns `false` for an unknown or closed reservation.
    pub fn commit(&mut self, id: ReservationId, actual: u64) -> bool {
        let Some((r, _)) = self.open.remove(&id) else {
            return false;
        };
        let used = self.used.entry(r).or_insert(0);
        *used = used.saturating_add(actual);
        true
    }

    /// Cancels a reservation without use.
    pub fn release(&mut self, id: ReservationId) -> bool {
        self.open.remove(&id).is_some()
    }

    /// True if committed use of any limited resource has reached its limit.
    #[must_use]
    pub fn exhausted(&self) -> bool {
        [
            Resource::Bytes,
            Resource::Files,
            Resource::WallMs,
            Resource::LlmUsdMicro,
        ]
        .into_iter()
        .any(|r| r.limit(&self.limits).is_some_and(|l| self.used(r) >= l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> Ledger {
        Ledger::new(Budgets {
            bytes: Some(100),
            llm_usd_micro: Some(2_500_000),
            ..Budgets::default()
        })
    }

    #[test]
    fn reservations_cannot_overspend_together() {
        let mut l = ledger();
        let a = l.reserve(Resource::Bytes, 60).unwrap();
        let err = l.reserve(Resource::Bytes, 50).unwrap_err();
        assert_eq!(err.available, 40);
        assert!(l.commit(a, 30));
        assert_eq!(l.available(Resource::Bytes), Some(70));
        assert!(
            !l.commit(a, 30),
            "a closed reservation cannot be committed twice"
        );
    }

    #[test]
    fn unlimited_resources_always_fit() {
        let mut l = ledger();
        assert!(l.reserve(Resource::Files, u64::MAX).is_ok());
        assert_eq!(l.available(Resource::Files), None);
    }

    #[test]
    fn actual_use_can_exceed_the_reservation_and_exhaust() {
        let mut l = ledger();
        let a = l.reserve(Resource::LlmUsdMicro, 1_000_000).unwrap();
        assert!(l.commit(a, 2_600_000));
        assert!(l.exhausted());
        assert_eq!(
            l.reserve(Resource::LlmUsdMicro, 1).unwrap_err().available,
            0
        );
    }

    #[test]
    fn release_returns_the_reservation() {
        let mut l = ledger();
        let a = l.reserve(Resource::Bytes, 100).unwrap();
        assert!(l.release(a));
        assert_eq!(l.available(Resource::Bytes), Some(100));
    }
}
