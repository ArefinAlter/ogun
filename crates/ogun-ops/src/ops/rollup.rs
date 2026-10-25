//! `window.rollup`: integer statistics over sealed window batches (O-09 section 6.1).
//!
//! Each input is JSON Lines of window records `{"key", "event_ms", "seq", "payload"}` (any JSON Lines
//! rows work; fields missing at the top level are read from `payload`). Rows are grouped by the `group_by`
//! fields (default `["key"]`) and, with `to_ms`, by the bucket `[floor(t / to_ms) * to_ms, + to_ms)` of their
//! `time_field` (default `event_ms`). For every numeric value field (default: every top-level number in
//! `payload` that is not a group field) the requested aggregates are computed as integers:
//!
//! | Aggregate | Value |
//! |---|---|
//! | `count` | Rows with a value. |
//! | `min`, `max`, `sum` | Over the scaled values. |
//! | `mean` | `round(sum * mean_scale / count)`, half away from zero; `mean_scale` (default 1000) is in the row. |
//! | `p50`, `p90`, `p95`, `p99` | Nearest-rank percentile of the scaled values. |
//!
//! Payloads may hold floats (O-09 section 3.1); a field with any float is scaled by `float_scale` (default
//! 1000) and rounded, and its `scale` is reported, so the output holds no floating-point number. Output:
//! one `rollup.jsonl` on `artifacts`, rows in JCS form sorted by bucket and group.

use std::collections::BTreeMap;
use std::io::BufRead;

use serde_json::{Map, Value, json};

use super::{CoreResult, Job};
use crate::OutputFile;
use crate::error::{OpResult, config, input as input_err, io as io_err, limit};
use crate::files;
use crate::metadata::record_field;

/// Most values kept in memory for one attempt.
const MAX_VALUES: usize = 20_000_000;
const AGGREGATES: &[&str] = &[
    "count", "min", "max", "sum", "mean", "p50", "p90", "p95", "p99",
];

#[derive(Debug, Clone, Copy)]
enum Num {
    I(i64),
    F(f64),
}

/// `(bucket start, group key as JSON)` to the group's key members and values.
type Groups = BTreeMap<(Option<i64>, String), (Map<String, Value>, Group)>;

#[derive(Default)]
struct Group {
    rows: u64,
    values: BTreeMap<String, Vec<Num>>,
}

#[allow(clippy::cast_possible_truncation)]
fn round_half_away(x: f64) -> i64 {
    x.round() as i64
}

fn div_round(num: i128, den: i128) -> i128 {
    if den == 0 {
        return 0;
    }
    let q = num / den;
    let r = num % den;
    if 2 * r.abs() >= den.abs() {
        q + if (num < 0) == (den < 0) { 1 } else { -1 }
    } else {
        q
    }
}

/// `window.rollup@1`: `{"group_by": [...], "to_ms": N, "time_field": "event_ms", "fields": [...],
/// "aggregates": [...], "mean_scale": 1000, "float_scale": 1000}`.
///
/// # Errors
///
/// `op.config_invalid`, `op.input_invalid`, `op.limit_exceeded`.
#[allow(clippy::too_many_lines)]
pub fn rollup(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&[
        "group_by",
        "to_ms",
        "time_field",
        "fields",
        "aggregates",
        "mean_scale",
        "float_scale",
    ])?;
    let group_by = job
        .strings("group_by")?
        .unwrap_or_else(|| vec!["key".into()]);
    if group_by.len() > 8 {
        return Err(config("window.rollup: at most 8 group_by fields").into());
    }
    let to_ms = job.u64("to_ms")?;
    if to_ms == Some(0) {
        return Err(config("window.rollup: to_ms must be at least 1").into());
    }
    let time_field = job.str("time_field")?.unwrap_or("event_ms").to_owned();
    let fields = job.strings("fields")?;
    let aggregates = job.strings("aggregates")?.unwrap_or_else(|| {
        ["count", "min", "max", "sum", "mean"]
            .map(String::from)
            .to_vec()
    });
    if let Some(bad) = aggregates
        .iter()
        .find(|a| !AGGREGATES.contains(&a.as_str()))
    {
        return Err(config(format!(
            "window.rollup: unknown aggregate {bad}; known: {}",
            AGGREGATES.join(", ")
        ))
        .into());
    }
    let mean_scale = i128::from(job.u64("mean_scale")?.unwrap_or(1000).max(1));
    let float_scale = job.u64("float_scale")?.unwrap_or(1000).max(1);
    #[allow(clippy::cast_precision_loss)]
    let float_scale_f = float_scale as f64;

    // (bucket start, group key JCS) -> group
    let mut groups: Groups = BTreeMap::new();
    let mut kept = 0usize;
    let mut records = 0u64;
    for (i, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        if !files::is_jsonl(&input.media_type, &input.path) && !input.media_type.contains("json") {
            return Err(
                input_err(format!("window.rollup: input {} is not JSON Lines", i + 1)).into(),
            );
        }
        let f = std::fs::File::open(&input.path).map_err(|e| io_err("reading an input", &e))?;
        for (n, line) in std::io::BufReader::new(f).lines().enumerate() {
            let line = line.map_err(|e| io_err("reading an input", &e))?;
            if line.trim().is_empty() {
                continue;
            }
            let row: Value = serde_json::from_str(&line).map_err(|_| {
                input_err(format!(
                    "window.rollup: row {} of input {} is not JSON",
                    n + 1,
                    i + 1
                ))
            })?;
            records += 1;
            let bucket = match to_ms {
                Some(size) => {
                    let t = record_field(&row, &time_field)
                        .and_then(Value::as_i64)
                        .ok_or_else(|| {
                            input_err(format!(
                                "window.rollup: row {} has no integer {time_field}",
                                n + 1
                            ))
                        })?;
                    let size = i64::try_from(size).unwrap_or(i64::MAX);
                    Some(t.div_euclid(size) * size)
                }
                None => None,
            };
            let mut key = Map::new();
            for g in &group_by {
                key.insert(
                    g.clone(),
                    record_field(&row, g).cloned().unwrap_or(Value::Null),
                );
            }
            let key_text = serde_json::to_string(&key).unwrap_or_default();
            let entry = groups
                .entry((bucket, key_text))
                .or_insert_with(|| (key.clone(), Group::default()));
            entry.1.rows += 1;
            let mut push = |name: &str, v: &Value| {
                let num = if let Some(x) = v.as_i64() {
                    Some(Num::I(x))
                } else if v.is_u64() {
                    None
                } else {
                    v.as_f64().filter(|f| f.is_finite()).map(Num::F)
                };
                if let Some(num) = num {
                    entry.1.values.entry(name.to_owned()).or_default().push(num);
                    kept += 1;
                }
            };
            match &fields {
                Some(list) => {
                    for f in list {
                        if let Some(v) = record_field(&row, f) {
                            push(f, v);
                        }
                    }
                }
                None => {
                    let payload = row.get("payload").unwrap_or(&row);
                    if let Some(obj) = payload.as_object() {
                        for (k, v) in obj {
                            if v.is_number()
                                && !group_by.contains(k)
                                && *k != time_field
                                && k != "seq"
                            {
                                push(k, v);
                            }
                        }
                    }
                }
            }
            if kept > MAX_VALUES {
                return Err(
                    limit("window.rollup: more than 20 million values in one attempt").into(),
                );
            }
        }
    }

    // One scale per field across all groups, so rows compare.
    let mut scale: BTreeMap<String, u64> = BTreeMap::new();
    for (_, g) in groups.values() {
        for (name, vals) in &g.values {
            let s = scale.entry(name.clone()).or_insert(1);
            if vals.iter().any(|v| matches!(v, Num::F(_))) {
                *s = float_scale;
            }
        }
    }
    let mut lines: Vec<u8> = Vec::new();
    for ((bucket, _), (key, g)) in &groups {
        let mut field_stats = Map::new();
        for (name, vals) in &g.values {
            let s = scale.get(name).copied().unwrap_or(1);
            let mut scaled: Vec<i64> = vals
                .iter()
                .map(|v| match *v {
                    Num::I(x) if s == 1 => x,
                    Num::I(x) => x.saturating_mul(i64::try_from(s).unwrap_or(1)),
                    Num::F(f) => round_half_away(f * if s == 1 { 1.0 } else { float_scale_f }),
                })
                .collect();
            scaled.sort_unstable();
            let count = scaled.len();
            let sum: i128 = scaled.iter().map(|v| i128::from(*v)).sum();
            let mut st = Map::new();
            st.insert("scale".into(), json!(s));
            for a in &aggregates {
                let v: Value = match a.as_str() {
                    "count" => json!(count),
                    "min" => json!(scaled.first()),
                    "max" => json!(scaled.last()),
                    "sum" => {
                        i64::try_from(sum).map_or_else(|_| json!(sum.to_string()), |v| json!(v))
                    }
                    "mean" => {
                        let m =
                            div_round(sum * mean_scale, i128::try_from(count).unwrap_or(1).max(1));
                        st.insert("mean_scale".into(), json!(mean_scale as u64));
                        i64::try_from(m).map_or_else(|_| json!(m.to_string()), |v| json!(v))
                    }
                    p => {
                        let pct: usize = p[1..].parse().unwrap_or(50);
                        let rank = (pct * count).div_ceil(100).max(1);
                        json!(scaled.get(rank - 1))
                    }
                };
                st.insert(a.clone(), v);
            }
            field_stats.insert(name.clone(), Value::Object(st));
        }
        let mut row = json!({"group": key, "records": g.rows, "fields": field_stats});
        if let (Some(b), Some(size)) = (bucket, to_ms) {
            row["bucket_start_ms"] = json!(b);
            row["bucket_end_ms"] = json!(b.saturating_add(i64::try_from(size).unwrap_or(i64::MAX)));
        }
        lines.extend(
            zeus_verify::canonical::to_jcs_bytes(&row)
                .map_err(|_| crate::error::internal("a rollup row cannot be canonicalised"))?,
        );
        lines.push(b'\n');
    }
    let path = job.out_dir("artifacts")?.join("rollup.jsonl");
    std::fs::write(&path, &lines).map_err(|e| io_err("writing the rollup", &e))?;
    let mut md = json!({"groups": groups.len(), "records": records});
    if !job.run.partition.is_empty() {
        md["window"] = json!(job.run.partition);
    }
    Ok(vec![OutputFile {
        port: "artifacts".into(),
        path,
        media_type: "application/x-ndjson".into(),
        metadata: Some(md),
    }])
}

/// The mean rule of this module, for one list of values.
///
/// # Errors
///
/// `op.limit_exceeded` when the mean does not fit in 64 bits.
pub fn mean_of(values: &[i64], scale: u64) -> OpResult<i64> {
    let sum: i128 = values.iter().map(|v| i128::from(*v)).sum();
    let n = i128::try_from(values.len()).unwrap_or(1).max(1);
    i64::try_from(div_round(sum * i128::from(scale), n)).map_err(|_| limit("mean out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding() {
        assert_eq!(mean_of(&[1, 2], 1).unwrap(), 2); // 1.5 -> 2
        assert_eq!(mean_of(&[-1, -2], 1).unwrap(), -2);
        assert_eq!(mean_of(&[1, 2, 2], 1000).unwrap(), 1667);
        assert_eq!(div_round(7, 2), 4);
    }
}
