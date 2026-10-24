//! Partition keys and windows (O-04 section 3).
//!
//! A time partition is complete when its window has ended: the scheduler never runs a partition
//! whose data cannot exist yet. Static partitions are always complete.

use ogun_ir::model::{Granularity, Partitions};

use crate::civil::{
    DAY_MS, Date, civil_from_days, days_from_civil, days_in_month, format_day, format_month,
    format_window_key, parse_day, parse_month, parse_window_key,
};

/// Most partitions one enumeration may return. Larger backfills are split by range.
pub const MAX_PARTITIONS: usize = 100_000;

/// A partition request the declaration cannot satisfy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PartitionError {
    /// A key is not of the declared form or not in the declared set.
    #[error("{0} is not a partition of this workflow")]
    UnknownKey(String),
    /// The range is reversed.
    #[error("the range ends before it starts")]
    Reversed,
    /// The range holds more than [`MAX_PARTITIONS`].
    #[error("the range holds more than {MAX_PARTITIONS} partitions; split it")]
    TooMany,
    /// Window partitions are created by their stream, not by the clock; list them from the run
    /// store instead.
    #[error("window partitions are created by the stream; list them from the run store")]
    Stream,
}

/// The half-open window `[start_ms, end_ms)` of a time partition, in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Inclusive start.
    pub start_ms: i64,
    /// Exclusive end.
    pub end_ms: i64,
}

/// A position on the partition axis: day number, month number, or index into the static keys.
fn ordinal(p: &Partitions, key: &str) -> Option<i64> {
    match p {
        Partitions::Time {
            granularity: Granularity::Day,
            ..
        } => parse_day(key).map(days_from_civil),
        Partitions::Time {
            granularity: Granularity::Month,
            ..
        } => parse_month(key).map(|(y, m)| i64::from(y) * 12 + i64::from(m) - 1),
        Partitions::Static { keys } => keys
            .iter()
            .position(|k| k == key)
            .and_then(|i| i64::try_from(i).ok()),
        Partitions::Window(w) => {
            let size = i64::try_from(w.size_ms).ok().filter(|s| *s > 0)?;
            let start = parse_window_key(key)?;
            (start % size == 0).then_some(start / size)
        }
    }
}

fn key_at(p: &Partitions, ordinal: i64) -> String {
    match p {
        Partitions::Time {
            granularity: Granularity::Day,
            ..
        } => format_day(civil_from_days(ordinal)),
        Partitions::Time {
            granularity: Granularity::Month,
            ..
        } => {
            let year = i32::try_from(ordinal.div_euclid(12)).unwrap_or(i32::MAX);
            let month = u32::try_from(ordinal.rem_euclid(12) + 1).unwrap_or(1);
            format_month(year, month)
        }
        Partitions::Static { keys } => usize::try_from(ordinal)
            .ok()
            .and_then(|i| keys.get(i))
            .cloned()
            .unwrap_or_default(),
        Partitions::Window(w) => {
            format_window_key(ordinal.saturating_mul(i64::try_from(w.size_ms).unwrap_or(1)))
        }
    }
}

/// The window of a time partition key; `None` for static partitions or an invalid key.
#[must_use]
pub fn window(p: &Partitions, key: &str) -> Option<Window> {
    match p {
        Partitions::Time {
            granularity: Granularity::Day,
            ..
        } => {
            let start = days_from_civil(parse_day(key)?) * DAY_MS;
            Some(Window {
                start_ms: start,
                end_ms: start + DAY_MS,
            })
        }
        Partitions::Time {
            granularity: Granularity::Month,
            ..
        } => {
            let (year, month) = parse_month(key)?;
            let first = days_from_civil(Date {
                year,
                month,
                day: 1,
            });
            let days = i64::from(days_in_month(year, month));
            Some(Window {
                start_ms: first * DAY_MS,
                end_ms: (first + days) * DAY_MS,
            })
        }
        Partitions::Static { .. } => None,
        Partitions::Window(w) => {
            let start = ordinal(p, key)?.checked_mul(i64::try_from(w.size_ms).ok()?)?;
            Some(Window {
                start_ms: start,
                end_ms: start.checked_add(i64::try_from(w.size_ms).ok()?)?,
            })
        }
    }
}

/// First and last ordinal of the declaration, with the last capped at the last complete
/// partition at `now_ms`. `None` when no partition is complete yet.
fn bounds(p: &Partitions, now_ms: i64) -> Option<(i64, i64)> {
    match p {
        Partitions::Time { start, end, .. } => {
            let first = ordinal(p, start)?;
            let declared_last = end.as_deref().and_then(|e| ordinal(p, e));
            // The last complete partition: its window ends at or before `now_ms`.
            let mut last = match p {
                Partitions::Time {
                    granularity: Granularity::Day,
                    ..
                } => now_ms.div_euclid(DAY_MS) - 1,
                _ => {
                    let today = civil_from_days(now_ms.div_euclid(DAY_MS));
                    i64::from(today.year) * 12 + i64::from(today.month) - 1 - 1
                }
            };
            if let Some(d) = declared_last {
                last = last.min(d);
            }
            (last >= first).then_some((first, last))
        }
        Partitions::Static { keys } => {
            let last = i64::try_from(keys.len()).ok()? - 1;
            (last >= 0).then_some((0, last))
        }
        // Windows are final once `end + allowed_lateness` has passed; the first one is whatever
        // the stream saw first, so ranges start at the caller's key.
        Partitions::Window(w) => {
            let size = i64::try_from(w.size_ms).ok().filter(|s| *s > 0)?;
            let lateness = i64::try_from(w.allowed_lateness_ms).ok()?;
            let last = (now_ms - lateness).div_euclid(size) - 1;
            Some((i64::MIN / 2, last))
        }
    }
}

/// Every complete partition key, in order.
///
/// # Errors
///
/// [`PartitionError::TooMany`] above [`MAX_PARTITIONS`].
pub fn complete(p: &Partitions, now_ms: i64) -> Result<Vec<String>, PartitionError> {
    if matches!(p, Partitions::Window(_)) {
        return Err(PartitionError::Stream);
    }
    match bounds(p, now_ms) {
        None => Ok(Vec::new()),
        Some((a, b)) => collect(p, a, b),
    }
}

/// The complete partitions from `from` to `to`, inclusive, in order.
///
/// # Errors
///
/// [`PartitionError::UnknownKey`] for keys outside the declaration, [`PartitionError::Reversed`],
/// and [`PartitionError::TooMany`].
pub fn range(
    p: &Partitions,
    from: &str,
    to: &str,
    now_ms: i64,
) -> Result<Vec<String>, PartitionError> {
    let a = ordinal(p, from).ok_or_else(|| PartitionError::UnknownKey(from.to_owned()))?;
    let b = ordinal(p, to).ok_or_else(|| PartitionError::UnknownKey(to.to_owned()))?;
    if b < a {
        return Err(PartitionError::Reversed);
    }
    let Some((first, last)) = bounds(p, now_ms) else {
        return Ok(Vec::new());
    };
    if a < first {
        return Err(PartitionError::UnknownKey(from.to_owned()));
    }
    let (a, b) = (a.max(first), b.min(last));
    if b < a {
        return Ok(Vec::new());
    }
    collect(p, a, b)
}

fn collect(p: &Partitions, a: i64, b: i64) -> Result<Vec<String>, PartitionError> {
    let n = usize::try_from(b - a + 1).map_err(|_| PartitionError::TooMany)?;
    if n > MAX_PARTITIONS {
        return Err(PartitionError::TooMany);
    }
    Ok((a..=b).map(|o| key_at(p, o)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daily(start: &str, end: Option<&str>) -> Partitions {
        Partitions::Time {
            granularity: Granularity::Day,
            start: start.into(),
            end: end.map(Into::into),
        }
    }

    fn monthly(start: &str) -> Partitions {
        Partitions::Time {
            granularity: Granularity::Month,
            start: start.into(),
            end: None,
        }
    }

    /// 2026-10-23T08:00:00Z
    const NOW: i64 = 1_792_713_600_000 + 8 * 3_600_000;

    #[test]
    fn only_complete_partitions_are_listed() {
        let keys = complete(&daily("2026-10-20", None), NOW).unwrap();
        assert_eq!(keys, ["2026-10-20", "2026-10-21", "2026-10-22"]);
        let keys = complete(&monthly("2026-08"), NOW).unwrap();
        assert_eq!(keys, ["2026-08", "2026-09"]);
        assert!(
            complete(&daily("2026-10-23", None), NOW)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn declared_end_caps_the_range() {
        let keys = complete(&daily("2024-02-27", Some("2024-03-01")), NOW).unwrap();
        assert_eq!(
            keys,
            ["2024-02-27", "2024-02-28", "2024-02-29", "2024-03-01"]
        );
    }

    #[test]
    fn ranges() {
        let p = daily("2020-01-01", None);
        assert_eq!(
            range(&p, "2020-12-31", "2021-01-01", NOW).unwrap(),
            ["2020-12-31", "2021-01-01"]
        );
        assert_eq!(
            range(&p, "2019-12-31", "2020-01-01", NOW),
            Err(PartitionError::UnknownKey("2019-12-31".into()))
        );
        assert_eq!(
            range(&p, "2020-01-02", "2020-01-01", NOW),
            Err(PartitionError::Reversed)
        );
        assert_eq!(
            range(&p, "2020-01-01", "2026-12-31", NOW).unwrap().len(),
            2487
        );
        let s = Partitions::Static {
            keys: vec!["dhaka".into(), "ctg".into(), "sylhet".into()],
        };
        assert_eq!(range(&s, "ctg", "sylhet", NOW).unwrap(), ["ctg", "sylhet"]);
    }

    #[test]
    fn window_partitions_come_from_the_stream() {
        let p = Partitions::Window(ogun_ir::model::WindowPartitions {
            source: "s".into(),
            time_field: "ts_ms".into(),
            key_field: "device_id".into(),
            size_ms: 60_000,
            allowed_lateness_ms: 3_600_000,
            reseal_after_ms: 30_000,
            max_delay_ms: 0,
            idle_timeout_ms: 600_000,
            max_future_ms: 300_000,
        });
        assert_eq!(complete(&p, NOW), Err(PartitionError::Stream));
        // A downstream backfill over final windows: lateness of 1 h caps the range.
        let keys = range(&p, "w20261023T065800Z", "w20261023T090000Z", NOW).unwrap();
        assert_eq!(keys, ["w20261023T065800Z", "w20261023T065900Z"]);
        assert_eq!(
            window(&p, "w20261023T065900Z").map(|w| w.end_ms - w.start_ms),
            Some(60_000)
        );
        assert!(range(&p, "w20261023T065830Z", "w20261023T070000Z", NOW).is_err());
    }

    #[test]
    fn windows() {
        let w = window(&monthly("2024-01"), "2024-02").unwrap();
        assert_eq!(w.end_ms - w.start_ms, 29 * DAY_MS);
        let w = window(&daily("2020-01-01", None), "1970-01-02").unwrap();
        assert_eq!(
            w,
            Window {
                start_ms: DAY_MS,
                end_ms: 2 * DAY_MS
            }
        );
    }
}
