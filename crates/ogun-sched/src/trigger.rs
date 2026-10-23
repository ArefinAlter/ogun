//! The next start time of a workflow from its triggers (O-04 section 2).

use ogun_ir::model::Trigger;

use crate::cron::Cron;

/// The earliest time after `after_ms` at which any trigger fires, given when the previous run
/// started (`last_start_ms`, for interval triggers). Manual triggers never fire on their own.
/// Invalid cron expressions are ignored here; the validator rejects them first.
#[must_use]
pub fn next_fire(triggers: &[Trigger], after_ms: i64, last_start_ms: Option<i64>) -> Option<i64> {
    triggers
        .iter()
        .filter_map(|t| match t {
            // A manual trigger never fires on its own; a stream trigger keeps a continuous run
            // going instead of starting runs at times (O-09 section 7).
            Trigger::Manual | Trigger::Stream { .. } => None,
            Trigger::Cron { cron } => Cron::parse(cron).ok()?.next_after(after_ms),
            Trigger::Interval { interval_ms } => {
                let step = i64::try_from(*interval_ms).ok()?;
                let next = last_start_ms.map_or(after_ms, |last| last.saturating_add(step));
                Some(next.max(after_ms))
            }
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earliest_trigger_wins() {
        let triggers = vec![
            Trigger::Manual,
            Trigger::Cron {
                cron: "0 * * * *".into(),
            },
            Trigger::Interval {
                interval_ms: 600_000,
            },
        ];
        // Interval: last start 0, so due at 600 000; cron: next hour at 3 600 000.
        assert_eq!(next_fire(&triggers, 1, Some(0)), Some(600_000));
        // Overdue interval fires now.
        assert_eq!(
            next_fire(&triggers[2..], 5_000_000, Some(0)),
            Some(5_000_000)
        );
        assert_eq!(next_fire(&[Trigger::Manual], 0, None), None);
    }
}
