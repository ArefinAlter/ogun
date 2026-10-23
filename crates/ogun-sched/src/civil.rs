//! Proleptic Gregorian calendar arithmetic in UTC, without floating point.
//!
//! `days_from_civil` and `civil_from_days` are Howard Hinnant's algorithms
//! (<https://howardhinnant.github.io/date_algorithms.html>), exact for every date this crate uses.

/// Milliseconds in a day.
pub const DAY_MS: i64 = 86_400_000;

/// A calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    /// Year, 1 to 9999.
    pub year: i32,
    /// Month, 1 to 12.
    pub month: u32,
    /// Day of month, 1 to 31.
    pub day: u32,
}

/// Days since 1970-01-01 for a civil date.
#[must_use]
pub fn days_from_civil(date: Date) -> i64 {
    let y = i64::from(date.year) - i64::from(date.month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(date.month);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(date.day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date for days since 1970-01-01.
#[must_use]
pub fn civil_from_days(days: i64) -> Date {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Date {
        year: i32::try_from(year).unwrap_or(i32::MAX),
        month: u32::try_from(month).unwrap_or(1),
        day: u32::try_from(day).unwrap_or(1),
    }
}

/// Day of week, 0 = Sunday.
#[must_use]
pub fn weekday(days: i64) -> u32 {
    // 1970-01-01 was a Thursday (4).
    u32::try_from((days + 4).rem_euclid(7)).unwrap_or(0)
}

/// Days in a month.
#[must_use]
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Parses `YYYY-MM-DD` into a valid date.
#[must_use]
pub fn parse_day(s: &str) -> Option<Date> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, m) = parse_month(&s[..7])?;
    let d: u32 = digits(&s[8..10])?;
    (1..=days_in_month(y, m)).contains(&d).then_some(Date {
        year: y,
        month: m,
        day: d,
    })
}

/// Parses `YYYY-MM`.
#[must_use]
pub fn parse_month(s: &str) -> Option<(i32, u32)> {
    let b = s.as_bytes();
    if b.len() != 7 || b[4] != b'-' {
        return None;
    }
    let y: i32 = digits(&s[..4])?;
    let m: u32 = digits(&s[5..7])?;
    ((1..=9999).contains(&y) && (1..=12).contains(&m)).then_some((y, m))
}

fn digits<T: std::str::FromStr>(s: &str) -> Option<T> {
    if s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// Formats `YYYY-MM-DD`.
#[must_use]
pub fn format_day(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

/// Formats `YYYY-MM`.
#[must_use]
pub fn format_month(year: i32, month: u32) -> String {
    format!("{year:04}-{month:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_across_four_centuries() {
        for days in (-200_000..200_000).step_by(17) {
            assert_eq!(days_from_civil(civil_from_days(days)), days);
        }
        assert_eq!(
            days_from_civil(Date {
                year: 1970,
                month: 1,
                day: 1
            }),
            0
        );
        assert_eq!(
            days_from_civil(Date {
                year: 2000,
                month: 3,
                day: 1
            }),
            11_017
        );
        assert_eq!(
            weekday(days_from_civil(Date {
                year: 2026,
                month: 10,
                day: 23
            })),
            5
        );
    }

    #[test]
    fn parses_only_real_dates() {
        assert!(parse_day("2024-02-29").is_some());
        assert!(parse_day("2023-02-29").is_none());
        assert!(parse_day("2023-2-28").is_none());
        assert!(parse_month("2026-13").is_none());
    }
}
