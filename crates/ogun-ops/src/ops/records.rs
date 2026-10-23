//! `filter` and `partition`: operators that read JSON Lines rows (and sealed window batches, whose rows
//! keep their bytes, so a filtered batch stays sorted canonical JSON Lines, O-09 section 6.1).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::{CoreResult, Job, input_name};
use crate::OutputFile;
use crate::error::{OpResult, config, input as input_err, io as io_err, limit};
use crate::files;
use crate::metadata::{
    ArtifactSubject, RecordSubject, holds, merged, parse_predicate, record_field,
};
use crate::template::value_text;

/// Most distinct keys `partition` writes (the static-key limit of O-02).
const MAX_KEYS: usize = 10_000;

fn rows(path: &std::path::Path) -> OpResult<impl Iterator<Item = OpResult<(String, Value)>>> {
    let f = File::open(path).map_err(|e| io_err("reading an input", &e))?;
    Ok(BufReader::new(f)
        .lines()
        .enumerate()
        .filter_map(|(n, line)| match line {
            Err(e) => Some(Err(io_err("reading an input", &e))),
            Ok(l) if l.trim().is_empty() => None,
            Ok(l) => Some(
                serde_json::from_str::<Value>(&l)
                    .map(|v| (l, v))
                    .map_err(|_| input_err(format!("row {} of an input is not JSON", n + 1))),
            ),
        }))
}

/// `filter@1`: `{"when": <predicate>, "where": <predicate>, "drop_if_missing": ["<field>"]}`.
///
/// `when` is evaluated on each artifact (metadata, media type, size): a match continues, anything else
/// goes to `rejected`. `where` and `drop_if_missing` are evaluated on each row of a JSON Lines input
/// (for window records, a field missing at the top level is read from `payload`); kept rows go to
/// `artifacts`, the others to `rejected`.
///
/// # Errors
///
/// `op.config_invalid`, `op.input_invalid`.
pub fn filter(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["when", "where", "drop_if_missing"])?;
    let when = job.cfg().get("when").map(parse_predicate).transpose()?;
    let where_ = job.cfg().get("where").map(parse_predicate).transpose()?;
    let required = job.strings("drop_if_missing")?.unwrap_or_default();
    if when.is_none() && where_.is_none() && required.is_empty() {
        return Err(config("filter needs when, where or drop_if_missing").into());
    }
    let row_level = where_.is_some() || !required.is_empty();
    let mut out = Vec::new();
    let total = job.inputs.len() as u64;
    for (i, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        job.progress(i as u64, Some(total));
        if let Some(p) = &when {
            let subject = ArtifactSubject {
                metadata: input.metadata.as_ref(),
                media_type: &input.media_type,
                size_bytes: input.size_bytes,
            };
            if !holds(p, &subject) {
                out.push(job.pass_through(
                    input,
                    "rejected",
                    None,
                    merged(input.metadata.as_ref(), Map::new()),
                )?);
                continue;
            }
        }
        if !row_level {
            out.push(job.pass_through(
                input,
                "artifacts",
                None,
                merged(input.metadata.as_ref(), Map::new()),
            )?);
            continue;
        }
        if !files::is_jsonl(&input.media_type, &input.path) {
            return Err(input_err(format!(
                "filter: input {} is not JSON Lines, so where and drop_if_missing cannot apply",
                i + 1
            ))
            .into());
        }
        let name = input_name(input);
        let kept_path = files::unique_path(&job.out_dir("artifacts")?, &name);
        let mut kept =
            BufWriter::new(File::create(&kept_path).map_err(|e| io_err("creating an output", &e))?);
        let mut rejected: Option<(PathBuf, BufWriter<File>)> = None;
        let (mut n_kept, mut n_rejected) = (0u64, 0u64);
        for row in rows(&input.path)? {
            let (line, value) = row?;
            let subject = RecordSubject(&value);
            let ok = where_.as_ref().is_none_or(|p| holds(p, &subject))
                && required
                    .iter()
                    .all(|f| record_field(&value, f).is_some_and(|v| !v.is_null()));
            if ok {
                n_kept += 1;
                writeln!(kept, "{line}").map_err(|e| io_err("writing an output", &e))?;
            } else {
                n_rejected += 1;
                if rejected.is_none() {
                    let p = files::unique_path(&job.out_dir("rejected")?, &name);
                    let w = BufWriter::new(
                        File::create(&p).map_err(|e| io_err("creating an output", &e))?,
                    );
                    rejected = Some((p, w));
                }
                if let Some((_, w)) = rejected.as_mut() {
                    writeln!(w, "{line}").map_err(|e| io_err("writing an output", &e))?;
                }
            }
            if (n_kept + n_rejected) % 10_000 == 0 {
                job.check_cancel()?;
            }
        }
        kept.flush().map_err(|e| io_err("writing an output", &e))?;
        let mut extra = Map::new();
        extra.insert("rows".into(), json!(n_kept));
        extra.insert("rows_rejected".into(), json!(n_rejected));
        out.push(OutputFile {
            port: "artifacts".into(),
            path: kept_path,
            media_type: input.media_type.clone(),
            metadata: Some(merged(input.metadata.as_ref(), extra.clone())),
        });
        if let Some((p, mut w)) = rejected {
            w.flush().map_err(|e| io_err("writing an output", &e))?;
            out.push(OutputFile {
                port: "rejected".into(),
                path: p,
                media_type: input.media_type.clone(),
                metadata: Some(merged(input.metadata.as_ref(), extra)),
            });
        }
    }
    job.progress(total, Some(total));
    Ok(out)
}

fn key_text(raw: &str) -> String {
    let mut k: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if k.is_empty() || k.starts_with('.') {
        k.insert(0, '_');
        k.truncate(64);
    }
    k
}

/// `partition@1`: exactly one of `{"by": "<field>"}`, `{"time_field": "<field>", "granularity": "day" |
/// "month"}` (Unix ms, UTC; keys as O-02 time partitions) or `{"rows": N}`. Reads JSON Lines inputs and
/// writes one `<key>.jsonl` per key with metadata `partition_key` and `rows`. A row without the field goes
/// to the key `_missing`. Keys are cut to `[A-Za-z0-9._-]{1,64}`.
///
/// # Errors
///
/// `op.config_invalid`, `op.input_invalid`, `op.limit_exceeded` beyond 10 000 keys.
pub fn partition(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["by", "time_field", "granularity", "rows"])?;
    enum Mode {
        By(String),
        Time(String, bool),
        Rows(u64),
    }
    let mode = match (job.str("by")?, job.str("time_field")?, job.u64("rows")?) {
        (Some(f), None, None) => Mode::By(f.to_owned()),
        (None, Some(f), None) => {
            let month = match job.str("granularity")? {
                None | Some("day") => false,
                Some("month") => true,
                Some(_) => return Err(config("partition: granularity must be day or month").into()),
            };
            Mode::Time(f.to_owned(), month)
        }
        (None, None, Some(n)) if n > 0 => Mode::Rows(n),
        _ => {
            return Err(config(
                "partition needs exactly one of by, time_field or rows (at least 1)",
            )
            .into());
        }
    };
    let dir = job.out_dir("artifacts")?;
    let mut writers: BTreeMap<String, (BufWriter<File>, u64, PathBuf)> = BTreeMap::new();
    let mut index = 0u64;
    for (i, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        if !files::is_jsonl(&input.media_type, &input.path) {
            return Err(input_err(format!("partition: input {} is not JSON Lines", i + 1)).into());
        }
        for row in rows(&input.path)? {
            let (line, value) = row?;
            let key = match &mode {
                Mode::By(f) => record_field(&value, f)
                    .and_then(value_text)
                    .map(|s| key_text(&s)),
                Mode::Time(f, month) => record_field(&value, f).and_then(Value::as_i64).map(|ms| {
                    let (y, m, d) = crate::context::civil_from_ms(ms);
                    if *month {
                        format!("{y:04}-{m:02}")
                    } else {
                        format!("{y:04}-{m:02}-{d:02}")
                    }
                }),
                Mode::Rows(n) => Some(format!("{:06}", index / n)),
            }
            .unwrap_or_else(|| "_missing".to_owned());
            index += 1;
            if !writers.contains_key(&key) {
                if writers.len() >= MAX_KEYS {
                    return Err(limit("partition: more than 10000 keys").into());
                }
                let path = dir.join(format!("{key}.jsonl"));
                let f = File::create(&path).map_err(|e| io_err("creating an output", &e))?;
                writers.insert(key.clone(), (BufWriter::new(f), 0, path));
            }
            if let Some((w, n, _)) = writers.get_mut(&key) {
                writeln!(w, "{line}").map_err(|e| io_err("writing an output", &e))?;
                *n += 1;
            }
            if index.is_multiple_of(10_000) {
                job.check_cancel()?;
            }
        }
    }
    let mut out = Vec::with_capacity(writers.len());
    for (key, (mut w, n, path)) in writers {
        w.flush().map_err(|e| io_err("writing an output", &e))?;
        out.push(OutputFile {
            port: "artifacts".into(),
            path,
            media_type: "application/x-ndjson".into(),
            metadata: Some(json!({"partition_key": key, "rows": n})),
        });
    }
    Ok(out)
}
