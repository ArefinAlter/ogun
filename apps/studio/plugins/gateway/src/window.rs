//! Segment keys on the gateway's receive clock: `w<YYYYMMDD>T<HHMMSS>Z` for the segment start, in UTC, the
//! partition key form of O-04 and O-11.

/// The start of the segment that holds `ms`.
#[must_use]
pub fn segment_start(ms: i64, segment_ms: u64) -> i64 {
    let size = i64::try_from(segment_ms).unwrap_or(i64::MAX).max(1);
    ms.div_euclid(size) * size
}

/// Days since 1970-01-01 to a civil date (H. Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// The key of the segment starting at `start_ms`.
#[must_use]
pub fn key(start_ms: i64) -> String {
    let secs = start_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "w{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_utc_and_aligned() {
        // 2026-10-23T08:17:42.500Z
        let ms = 1_792_743_462_500;
        let start = segment_start(ms, 300_000);
        assert_eq!(key(start), "w20261023T081500Z");
        assert_eq!(key(0), "w19700101T000000Z");
        assert_eq!(key(segment_start(-1, 60_000)), "w19691231T235900Z");
    }
}
