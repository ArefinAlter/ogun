//! `parse@1`: JSON, CSV or JSON Lines documents into JSON Lines rows, the form that `filter`, `partition` and
//! `window.rollup` read (ADR 0016 adds this one operator to the closed catalogue).
//!
//! | Member | Meaning |
//! |---|---|
//! | `format` | `json`, `csv` or `jsonl`; by default from the input's media type, then its extension. |
//! | `pointer` | An RFC 6901 JSON Pointer to the part of a JSON document that holds the rows (`/data/items`). Empty: the whole document. |
//! | `delimiter` | One character for `csv`; default `,`. |
//! | `max_rows` | Stop with `op.limit_exceeded` beyond this many rows in one input (default 1 000 000, at most 10 000 000). |
//!
//! A JSON array gives one row per element, an object one row, and a scalar element is wrapped as
//! `{"value": ...}` so every row is an object. A CSV input needs a header row; every field is text (a later
//! operator or a code operator decides what a column means). The output is one `.jsonl` file per input with
//! metadata `rows` and `source_blake3`; the input's own metadata is kept.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use serde_json::{Map, Value, json};

use super::{CoreResult, Job, input_name};
use crate::OutputFile;
use crate::error::{OpResult, config, input as input_err, io as io_err, limit};
use crate::files;
use crate::metadata::merged;

/// Largest input read into memory for JSON (CSV and JSON Lines stream).
const MAX_JSON_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_ROWS: u64 = 1_000_000;
const HARD_MAX_ROWS: u64 = 10_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Csv,
    Jsonl,
}

fn format_of(configured: Option<&str>, media_type: &str, name: &str) -> OpResult<Format> {
    match configured {
        Some("json") => return Ok(Format::Json),
        Some("csv") => return Ok(Format::Csv),
        Some("jsonl") => return Ok(Format::Jsonl),
        Some(_) => return Err(config("parse: format must be json, csv or jsonl")),
        None => {}
    }
    let mt = media_type.to_ascii_lowercase();
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    Ok(if mt.contains("csv") || ext == "csv" || ext == "tsv" {
        Format::Csv
    } else if mt.contains("ndjson") || mt.contains("jsonl") || ext == "jsonl" || ext == "ndjson" {
        Format::Jsonl
    } else if mt.contains("json") || ext == "json" {
        Format::Json
    } else {
        return Err(input_err(format!(
            "parse: cannot tell the format of {name}; set format to json, csv or jsonl"
        )));
    })
}

fn as_row(v: Value) -> Value {
    match v {
        Value::Object(_) => v,
        other => json!({ "value": other }),
    }
}

/// Splits one CSV record from `text` starting at `pos` (RFC 4180: quoted fields, doubled quotes, CR LF).
/// Returns the fields and the position after the record's line break; `None` at the end of the text.
fn csv_record(text: &str, mut pos: usize, delimiter: char) -> Option<(Vec<String>, usize)> {
    let bytes = text.as_bytes();
    if pos >= bytes.len() {
        return None;
    }
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text[pos..].char_indices().peekable();
    let mut consumed = 0usize;
    while let Some((i, c)) = chars.next() {
        consumed = i + c.len_utf8();
        if in_quotes {
            if c == '"' {
                if chars.peek().is_some_and(|(_, n)| *n == '"') {
                    field.push('"');
                    let (j, n) = chars.next().unwrap_or((i, '"'));
                    consumed = j + n.len_utf8();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            in_quotes = true;
        } else if c == delimiter {
            fields.push(std::mem::take(&mut field));
        } else if c == '\n' {
            fields.push(field);
            pos += consumed;
            return Some((fields, pos));
        } else if c == '\r' {
            if chars.peek().is_some_and(|(_, n)| *n == '\n') {
                let (j, n) = chars.next().unwrap_or((i, '\n'));
                consumed = j + n.len_utf8();
            }
            fields.push(field);
            pos += consumed;
            return Some((fields, pos));
        } else {
            field.push(c);
        }
    }
    fields.push(field);
    Some((fields, pos + consumed))
}

fn write_row(w: &mut BufWriter<File>, row: &Value) -> OpResult<()> {
    serde_json::to_writer(&mut *w, row)
        .map_err(|e| io_err("writing rows", &std::io::Error::from(e)))?;
    w.write_all(b"\n").map_err(|e| io_err("writing rows", &e))
}

/// `parse@1`.
///
/// # Errors
///
/// `op.config_invalid`, `op.input_invalid` for a document that does not parse, `op.limit_exceeded`.
pub fn parse(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["format", "pointer", "delimiter", "max_rows"])?;
    let configured = job.str("format")?;
    let pointer = job.str("pointer")?.unwrap_or("");
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Err(config("parse: pointer must be empty or start with /").into());
    }
    let delimiter = match job.str("delimiter")? {
        None => ',',
        Some(d) if d.chars().count() == 1 => d.chars().next().unwrap_or(','),
        Some(_) => return Err(config("parse: delimiter must be one character").into()),
    };
    let max_rows = job.u64("max_rows")?.unwrap_or(DEFAULT_MAX_ROWS);
    if !(1..=HARD_MAX_ROWS).contains(&max_rows) {
        return Err(config("parse: max_rows must be 1 to 10000000").into());
    }
    let dir = job.out_dir("artifacts")?;
    let total = job.inputs.len() as u64;
    let mut out = Vec::new();
    for (i, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        let name = input_name(input);
        let format = format_of(configured, &input.media_type, &name)?;
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
        let path = files::unique_path(
            &dir,
            &format!("{}.jsonl", files::sanitize_name(stem, "rows")),
        );
        let mut w =
            BufWriter::new(File::create(&path).map_err(|e| io_err("creating an output", &e))?);
        let mut rows = 0u64;
        let mut push = |w: &mut BufWriter<File>, row: Value| -> OpResult<()> {
            rows += 1;
            if rows > max_rows {
                return Err(limit(format!("parse: more than {max_rows} rows in {name}")));
            }
            write_row(w, &as_row(row))
        };
        match format {
            Format::Json => {
                if input.size_bytes > MAX_JSON_BYTES {
                    return Err(limit(format!("parse: {name} is larger than 256 MiB")).into());
                }
                let bytes =
                    std::fs::read(&input.path).map_err(|e| io_err("reading an input", &e))?;
                let doc: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| input_err(format!("parse: {name} is not valid JSON")))?;
                let part = if pointer.is_empty() {
                    Some(&doc)
                } else {
                    doc.pointer(pointer)
                };
                match part {
                    None => {
                        return Err(input_err(format!(
                            "parse: the pointer {pointer} does not exist in {name}"
                        ))
                        .into());
                    }
                    Some(Value::Array(items)) => {
                        for item in items {
                            push(&mut w, item.clone())?;
                        }
                    }
                    Some(other) => push(&mut w, other.clone())?,
                }
            }
            Format::Jsonl => {
                let f = File::open(&input.path).map_err(|e| io_err("reading an input", &e))?;
                for (n, line) in BufReader::new(f).lines().enumerate() {
                    let line = line.map_err(|e| io_err("reading an input", &e))?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let v: Value = serde_json::from_str(&line).map_err(|_| {
                        input_err(format!("parse: line {} of {name} is not JSON", n + 1))
                    })?;
                    push(&mut w, v)?;
                }
            }
            Format::Csv => {
                let bytes =
                    std::fs::read(&input.path).map_err(|e| io_err("reading an input", &e))?;
                if bytes.len() as u64 > MAX_JSON_BYTES {
                    return Err(limit(format!("parse: {name} is larger than 256 MiB")).into());
                }
                let text = String::from_utf8_lossy(&bytes);
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                let Some((header, mut pos)) = csv_record(text, 0, delimiter) else {
                    return Err(input_err(format!("parse: {name} is empty")).into());
                };
                if header.iter().any(String::is_empty) || header.len() > 1024 {
                    return Err(input_err(format!(
                        "parse: the header of {name} has an empty or too many column names"
                    ))
                    .into());
                }
                while let Some((fields, next)) = csv_record(text, pos, delimiter) {
                    pos = next;
                    if fields.len() == 1 && fields[0].is_empty() {
                        continue;
                    }
                    let mut row = Map::new();
                    for (h, v) in header.iter().zip(fields) {
                        row.insert(h.clone(), Value::String(v));
                    }
                    push(&mut w, Value::Object(row))?;
                }
            }
        }
        w.flush().map_err(|e| io_err("writing an output", &e))?;
        drop(w);
        out.push(OutputFile {
            port: "artifacts".into(),
            path,
            media_type: "application/x-ndjson".into(),
            metadata: Some(merged(
                input.metadata.as_ref(),
                json_map([
                    ("rows", json!(rows)),
                    ("source_blake3", json!(input.blake3)),
                ]),
            )),
        });
        job.progress(i as u64 + 1, Some(total));
    }
    Ok(out)
}

fn json_map<const N: usize>(items: [(&str, Value); N]) -> Map<String, Value> {
    items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

#[cfg(test)]
mod tests {
    use super::csv_record;

    fn all(text: &str) -> Vec<Vec<String>> {
        let mut pos = 0;
        let mut out = Vec::new();
        while let Some((f, next)) = csv_record(text, pos, ',') {
            out.push(f);
            pos = next;
        }
        out
    }

    #[test]
    fn csv_handles_quotes_line_breaks_and_crlf() {
        let rows = all("a,b\r\n1,\"x,\"\"y\"\"\nz\"\r\n2,\n");
        assert_eq!(rows[0], ["a", "b"]);
        assert_eq!(rows[1], ["1", "x,\"y\"\nz"]);
        assert_eq!(rows[2], ["2", ""]);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn csv_without_a_final_line_break_and_with_unicode() {
        let rows = all("n,é\n1,ü");
        assert_eq!(rows[1], ["1", "ü"]);
    }
}
