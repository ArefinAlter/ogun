//! Five-field cron expressions evaluated in UTC (O-04 section 2.1).
//!
//! Syntax as validated by O-02 section 4.1. Semantics follow Vixie cron: when both day of month and
//! day of week are restricted, a day matches if either matches; day of week 7 is Sunday.

use crate::civil::{DAY_MS, Date, civil_from_days, weekday};

const MINUTE_MS: i64 = 60_000;

/// A parsed cron schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u32,
    days_of_month: u32,
    months: u16,
    days_of_week: u8,
    dom_restricted: bool,
    dow_restricted: bool,
}

/// The expression is not valid cron.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a valid five-field cron expression")]
pub struct CronError;

fn field(text: &str, lo: u32, hi: u32) -> Result<(u64, bool), CronError> {
    let mut bits = 0u64;
    let num = |s: &str| -> Result<u32, CronError> {
        if s.is_empty() || s.len() > 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(CronError);
        }
        let n: u32 = s.parse().map_err(|_| CronError)?;
        if (lo..=hi).contains(&n) {
            Ok(n)
        } else {
            Err(CronError)
        }
    };
    for item in text.split(',') {
        let (base, step) = match item.split_once('/') {
            Some((b, s)) => {
                let s: u32 = s.parse().map_err(|_| CronError)?;
                if s == 0
                    || !item
                        .rsplit('/')
                        .next()
                        .is_some_and(|t| t.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Err(CronError);
                }
                (b, s)
            }
            None => (item, 1),
        };
        let (a, b) = if base == "*" {
            (lo, hi)
        } else if let Some((x, y)) = base.split_once('-') {
            let (x, y) = (num(x)?, num(y)?);
            if x > y {
                return Err(CronError);
            }
            (x, y)
        } else {
            let x = num(base)?;
            (x, if step > 1 { hi } else { x })
        };
        let mut v = a;
        while v <= b {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, text != "*"))
}

impl Cron {
    /// Parses `minute hour day-of-month month day-of-week`.
    ///
    /// # Errors
    ///
    /// [`CronError`] for anything O-02 section 4.1 does not allow.
    pub fn parse(text: &str) -> Result<Self, CronError> {
        let f: Vec<&str> = text.split_ascii_whitespace().collect();
        if f.len() != 5 {
            return Err(CronError);
        }
        let (minutes, _) = field(f[0], 0, 59)?;
        let (hours, _) = field(f[1], 0, 23)?;
        let (dom, dom_restricted) = field(f[2], 1, 31)?;
        let (months, _) = field(f[3], 1, 12)?;
        let (mut dow, dow_restricted) = field(f[4], 0, 7)?;
        if dow & (1 << 7) != 0 {
            dow |= 1;
        }
        Ok(Self {
            minutes,
            hours: u32::try_from(hours).map_err(|_| CronError)?,
            days_of_month: u32::try_from(dom).map_err(|_| CronError)?,
            months: u16::try_from(months).map_err(|_| CronError)?,
            days_of_week: u8::try_from(dow & 0x7f).map_err(|_| CronError)?,
            dom_restricted,
            dow_restricted,
        })
    }

    fn day_matches(&self, days: i64, date: Date) -> bool {
        let dom = self.days_of_month & (1 << date.day) != 0;
        let dow = self.days_of_week & (1 << weekday(days)) != 0;
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom || dow,
            (true, false) => dom,
            (false, true) => dow,
            (false, false) => true,
        }
    }

    /// The first fire time strictly after `after_ms`, at whole minutes, within eight years.
    /// `None` if the expression never fires in that span (for example `0 0 31 2 *`).
    #[must_use]
    pub fn next_after(&self, after_ms: i64) -> Option<i64> {
        let start = (after_ms.div_euclid(MINUTE_MS) + 1) * MINUTE_MS;
        let first_day = start.div_euclid(DAY_MS);
        for days in first_day..first_day + 8 * 366 {
            let date = civil_from_days(days);
            if self.months & (1 << date.month) == 0 || !self.day_matches(days, date) {
                continue;
            }
            let day_start = days * DAY_MS;
            for hour in 0..24u32 {
                if self.hours & (1 << hour) == 0 {
                    continue;
                }
                for minute in 0..60u32 {
                    if self.minutes & (1 << minute) == 0 {
                        continue;
                    }
                    let t = day_start + i64::from(hour) * 3_600_000 + i64::from(minute) * MINUTE_MS;
                    if t >= start {
                        return Some(t);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civil::{Date, days_from_civil};

    fn at(y: i32, m: u32, d: u32, hh: i64, mm: i64) -> i64 {
        days_from_civil(Date {
            year: y,
            month: m,
            day: d,
        }) * DAY_MS
            + hh * 3_600_000
            + mm * MINUTE_MS
    }

    #[test]
    fn sunday_three_am() {
        let c = Cron::parse("0 3 * * 0").unwrap();
        // Friday 2026-10-23 08:00 -> Sunday 2026-10-25 03:00.
        assert_eq!(
            c.next_after(at(2026, 10, 23, 8, 0)),
            Some(at(2026, 10, 25, 3, 0))
        );
        let c7 = Cron::parse("0 3 * * 7").unwrap();
        assert_eq!(
            c7.next_after(at(2026, 10, 23, 8, 0)),
            Some(at(2026, 10, 25, 3, 0))
        );
    }

    #[test]
    fn strictly_after_and_steps() {
        let c = Cron::parse("*/15 * * * *").unwrap();
        assert_eq!(
            c.next_after(at(2026, 1, 1, 0, 0)),
            Some(at(2026, 1, 1, 0, 15))
        );
        assert_eq!(
            c.next_after(at(2026, 1, 1, 0, 14) + 59_999),
            Some(at(2026, 1, 1, 0, 15))
        );
        let c = Cron::parse("30 4 * * *").unwrap();
        assert_eq!(
            c.next_after(at(2026, 12, 31, 5, 0)),
            Some(at(2027, 1, 1, 4, 30))
        );
    }

    #[test]
    fn day_of_month_or_day_of_week() {
        // The 13th or any Friday.
        let c = Cron::parse("0 0 13 * 5").unwrap();
        assert_eq!(
            c.next_after(at(2026, 10, 23, 1, 0)),
            Some(at(2026, 10, 30, 0, 0))
        );
        assert_eq!(
            c.next_after(at(2026, 11, 7, 0, 0)),
            Some(at(2026, 11, 13, 0, 0))
        );
    }

    #[test]
    fn leap_day_and_never() {
        let c = Cron::parse("0 0 29 2 *").unwrap();
        assert_eq!(
            c.next_after(at(2026, 1, 1, 0, 0)),
            Some(at(2028, 2, 29, 0, 0))
        );
        assert_eq!(Cron::parse("0 0 31 2 *").unwrap().next_after(0), None);
    }

    #[test]
    fn rejects_bad_syntax() {
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "*/0 * * * *",
            "5-1 * * * *",
            "a * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }
}
