//! Run-context placeholders in node configuration.
//!
//! A node's `config` may name the partition and run it is running for, for example
//! `"url": "https://example.org/data/{partition}.csv"` or `"as_of": "{run.date}"`. The registry expands
//! this closed set of placeholders in every configuration string before the operator sees it:
//!
//! | Placeholder | Value |
//! |---|---|
//! | `{partition}` | The partition key, `""` when unpartitioned. |
//! | `{partition.year}`, `{partition.month}`, `{partition.day}` | Parts of a time partition key or of a window's start (UTC). |
//! | `{partition.start_ms}`, `{partition.end_ms}` | The partition's time range, Unix ms, end exclusive. |
//! | `{partition.start_date}`, `{partition.end_date}` | The same range as `YYYY-MM-DD`, end exclusive. |
//! | `{run.id}` | The run id. |
//! | `{run.date}`, `{run.month}`, `{run.year}` | The UTC date the run was created (from the run id's ULID time). |
//! | `{run.start_ms}` | The run's creation time, Unix ms. |
//! | `{node.id}` | The node id. |
//!
//! Anything else in braces is left as written: item templates (`{filename|slug}`, Zeus 08 section 5.1)
//! are rendered later, per item, by the operators that support them. `{{` and `}}` are kept verbatim.
//! A placeholder of the closed set that has no value for this partition (a `{partition.year}` on a static
//! key) is `op.config_invalid`. Values inside `{"secret_ref": ...}` objects are never touched.

use serde_json::Value;

use crate::error::{OpResult, config};
use crate::{Invocation, PartitionWindow};

const DAY_MS: i64 = 86_400_000;

/// What the placeholders read.
#[derive(Debug, Clone)]
pub struct RunContext {
    /// Run id text.
    pub run_id: String,
    /// Run creation, Unix ms.
    pub run_ms: i64,
    /// Node id.
    pub node_id: String,
    /// Partition key.
    pub partition: String,
    /// The partition's range, when it has one.
    pub window: Option<PartitionWindow>,
}

impl RunContext {
    /// The context of one invocation.
    #[must_use]
    pub fn of(inv: &Invocation) -> Self {
        let run_ms = i64::try_from(inv.run_id.ulid().timestamp_ms()).unwrap_or(0);
        let window = inv.window.or_else(|| range_of_key(&inv.partition));
        Self {
            run_id: inv.run_id.to_string(),
            run_ms,
            node_id: inv.node_id.clone(),
            partition: inv.partition.clone(),
            window,
        }
    }

    fn value(&self, name: &str) -> Option<Result<String, ()>> {
        let date = |ms: i64| {
            let (y, m, d) = civil_from_ms(ms);
            format!("{y:04}-{m:02}-{d:02}")
        };
        let part = |f: fn((i64, u32, u32)) -> String| match self.window {
            Some(w) => Ok(f(civil_from_ms(w.start_ms))),
            None => Err(()),
        };
        Some(match name {
            "partition" => Ok(self.partition.clone()),
            "partition.year" => part(|(y, _, _)| format!("{y:04}")),
            "partition.month" => part(|(_, m, _)| format!("{m:02}")),
            "partition.day" => part(|(_, _, d)| format!("{d:02}")),
            "partition.start_ms" => self.window.map(|w| w.start_ms.to_string()).ok_or(()),
            "partition.end_ms" => self.window.map(|w| w.end_ms.to_string()).ok_or(()),
            "partition.start_date" => self.window.map(|w| date(w.start_ms)).ok_or(()),
            "partition.end_date" => self.window.map(|w| date(w.end_ms)).ok_or(()),
            "run.id" => Ok(self.run_id.clone()),
            "run.date" => Ok(date(self.run_ms)),
            "run.month" => {
                let (y, m, _) = civil_from_ms(self.run_ms);
                Ok(format!("{y:04}-{m:02}"))
            }
            "run.year" => Ok(format!("{:04}", civil_from_ms(self.run_ms).0)),
            "run.start_ms" => Ok(self.run_ms.to_string()),
            "node.id" => Ok(self.node_id.clone()),
            _ => return None,
        })
    }

    /// Expands the placeholders of the closed set in one string.
    ///
    /// # Errors
    ///
    /// `op.config_invalid` when a placeholder of the set has no value for this partition.
    pub fn expand_str(&self, text: &str) -> OpResult<String> {
        if !text.contains('{') {
            return Ok(text.to_owned());
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find(['{', '}']) {
            out.push_str(&rest[..i]);
            let tail = &rest[i..];
            if tail.starts_with("{{") || tail.starts_with("}}") {
                out.push_str(&tail[..2]);
                rest = &tail[2..];
                continue;
            }
            if let Some(after) = tail.strip_prefix('}') {
                out.push('}');
                rest = after;
                continue;
            }
            match tail[1..].find('}') {
                Some(end) => {
                    let name = &tail[1..=end];
                    match self.value(name) {
                        Some(Ok(v)) => out.push_str(&v),
                        Some(Err(())) => {
                            return Err(config(format!(
                                "the placeholder {{{name}}} has no value for this partition"
                            )));
                        }
                        None => out.push_str(&tail[..end + 2]),
                    }
                    rest = &tail[end + 2..];
                }
                None => {
                    out.push_str(tail);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        Ok(out)
    }

    /// Expands every string in a configuration value, except inside secret references.
    ///
    /// # Errors
    ///
    /// See [`RunContext::expand_str`].
    pub fn expand(&self, value: &Value) -> OpResult<Value> {
        Ok(match value {
            Value::String(s) => Value::String(self.expand_str(s)?),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|v| self.expand(v))
                    .collect::<OpResult<_>>()?,
            ),
            Value::Object(map) if crate::secrets::is_secret_ref(value) => {
                Value::Object(map.clone())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), self.expand(v)?)))
                    .collect::<OpResult<_>>()?,
            ),
            other => other.clone(),
        })
    }
}

/// The time range of a time partition key (`YYYY-MM-DD`, `YYYY-MM`) or a window key
/// (`wYYYYMMDDTHHMMSSZ`, without its size, which only the IR knows: the range is then one instant wide
/// and only the start is meaningful).
#[must_use]
pub fn range_of_key(key: &str) -> Option<PartitionWindow> {
    let num = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    if let Some(w) = key.strip_prefix('w') {
        if w.len() != 16 || !w.ends_with('Z') || w.as_bytes()[8] != b'T' {
            return None;
        }
        let (y, mo, d) = (num(&w[0..4])?, num(&w[4..6])?, num(&w[6..8])?);
        let (h, mi, s) = (num(&w[9..11])?, num(&w[11..13])?, num(&w[13..15])?);
        let start = ms_from_civil(y, u32::try_from(mo).ok()?, u32::try_from(d).ok()?)?
            + ((h * 60 + mi) * 60 + s) * 1000;
        return Some(PartitionWindow {
            start_ms: start,
            end_ms: start + 1,
        });
    }
    match key.len() {
        10 if key.as_bytes()[4] == b'-' && key.as_bytes()[7] == b'-' => {
            let start = ms_from_civil(
                num(&key[0..4])?,
                u32::try_from(num(&key[5..7])?).ok()?,
                u32::try_from(num(&key[8..10])?).ok()?,
            )?;
            Some(PartitionWindow {
                start_ms: start,
                end_ms: start + DAY_MS,
            })
        }
        7 if key.as_bytes()[4] == b'-' => {
            let (y, m) = (num(&key[0..4])?, u32::try_from(num(&key[5..7])?).ok()?);
            let start = ms_from_civil(y, m, 1)?;
            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            Some(PartitionWindow {
                start_ms: start,
                end_ms: ms_from_civil(ny, nm, 1)?,
            })
        }
        _ => None,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix ms of a UTC midnight, `None` for an impossible date.
#[must_use]
pub fn ms_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * DAY_MS)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// The UTC date of a Unix ms instant.
#[must_use]
pub fn civil_from_ms(ms: i64) -> (i64, u32, u32) {
    let z = ms.div_euclid(DAY_MS) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// RFC 3339 UTC with milliseconds (Zeus 00), for timestamps the operators write.
#[must_use]
pub fn rfc3339_ms(ms: i64) -> String {
    let (y, mo, d) = civil_from_ms(ms);
    let in_day = ms.rem_euclid(DAY_MS);
    let (h, mi, s, milli) = (
        in_day / 3_600_000,
        (in_day / 60_000) % 60,
        (in_day / 1000) % 60,
        in_day % 1000,
    );
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(partition: &str) -> RunContext {
        RunContext {
            run_id: "orun_01JB0000000000000000000000".into(),
            run_ms: 1_761_177_600_000, // 2025-10-23
            node_id: "fetch".into(),
            partition: partition.into(),
            window: range_of_key(partition),
        }
    }

    #[test]
    fn dates_round_trip() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2026, 10, 23), (1969, 12, 31)] {
            let ms = ms_from_civil(y, m, d).unwrap();
            assert_eq!(civil_from_ms(ms), (y, m, d));
        }
        assert!(ms_from_civil(2025, 2, 29).is_none());
        assert_eq!(rfc3339_ms(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn expands_the_closed_set_only() {
        let c = ctx("2019-03");
        assert_eq!(
            c.expand_str("y={partition.year}/m={partition.month}/{id}/{{x}}")
                .unwrap(),
            "y=2019/m=03/{id}/{{x}}"
        );
        assert_eq!(c.expand_str("{run.date}").unwrap(), "2025-10-23");
        assert!(ctx("static-a").expand_str("{partition.year}").is_err());
        assert_eq!(ctx("").expand_str("a{partition}b").unwrap(), "ab");
        let w = range_of_key("w20261023T081500Z").unwrap();
        assert_eq!(rfc3339_ms(w.start_ms), "2026-10-23T08:15:00.000Z");
    }
}
