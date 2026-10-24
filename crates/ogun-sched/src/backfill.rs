//! Backfill plans (O-04 section 4): which partitions of a range still need work.

use std::collections::BTreeSet;

use serde::Serialize;

/// What a backfill over a range will do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BackfillPlan {
    /// Partitions to run, in order.
    pub to_run: Vec<String>,
    /// Partitions whose outputs are already committed for this workflow hash.
    pub up_to_date: Vec<String>,
}

/// Splits `range` into partitions to run and partitions already done.
///
/// `done` holds the partitions that succeeded under the same workflow hash; a partition that
/// succeeded under another hash is run again, because the workflow that would produce it changed.
#[must_use]
pub fn plan(range: &[String], done: &BTreeSet<String>) -> BackfillPlan {
    let mut out = BackfillPlan::default();
    for key in range {
        if done.contains(key) {
            out.up_to_date.push(key.clone());
        } else {
            out.to_run.push(key.clone());
        }
    }
    out
}

/// Deals `to_run` round-robin into `lanes` lanes, so each lane works through the range in order
/// and the earliest partitions finish first.
#[must_use]
pub fn lanes(to_run: &[String], lanes: usize) -> Vec<Vec<String>> {
    let n = lanes.max(1);
    let mut out = vec![Vec::new(); n];
    for (i, key) in to_run.iter().enumerate() {
        out[i % n].push(key.clone());
    }
    out.retain(|l| !l.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_done_partitions_and_keeps_order() {
        let range: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        let done = BTreeSet::from(["b".to_owned()]);
        let p = plan(&range, &done);
        assert_eq!(p.to_run, ["a", "c", "d"]);
        assert_eq!(p.up_to_date, ["b"]);
        assert_eq!(lanes(&p.to_run, 2), vec![vec!["a", "d"], vec!["c"]]);
        assert_eq!(lanes(&p.to_run, 0), vec![vec!["a", "c", "d"]]);
        assert_eq!(lanes(&[], 4), Vec::<Vec<String>>::new());
    }
}
