//! `llm.enrich`: adds the fields of a schema to each row of JSON-lines or JSON inputs.

use ogun_llm::prompt::Document;
use ogun_llm::schema::wrap;
use ogun_llm::{LlmError, Schema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{by_index, rows_document};
use crate::common::{self, Ctx, Lines, read_rows};
use crate::protocol::Input;

fn default_batch() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// JSON Schema (`type: object` with `properties`) of the fields to add.
    schema: Value,
    /// Rows per call.
    #[serde(default = "default_batch")]
    batch_rows: u32,
    /// Replace members a row already has (default: keep them).
    #[serde(default)]
    overwrite: bool,
}

const TASK: &str = "Each document line is a JSON object with an `index` and a `row`. For every row, determine the \
fields described by the field schema below from the row's own content. Answer with a JSON object whose `items` member \
has exactly one entry per row: {\"index\": <the row's index>, \"fields\": <an object satisfying the field schema>}. \
Use only information in the row; use null where the schema allows it rather than guessing.";

pub async fn run(ctx: &mut Ctx, own: Map<String, Value>, inputs: &[Input]) -> Result<(), LlmError> {
    let cfg: Config = common::own(own)?;
    if !(1..=200).contains(&cfg.batch_rows) {
        return Err(LlmError::config(
            "config.batch_rows",
            "an integer from 1 to 200",
        ));
    }
    let fields = Schema::compile(&cfg.schema, "config.schema")?;
    if !cfg.schema["properties"].is_object() {
        return Err(LlmError::config(
            "config.schema",
            "an object schema with `properties` (the fields to add)",
        ));
    }
    let item = json!({
        "type": "object",
        "properties": { "index": { "type": "integer" }, "fields": fields.value() },
        "required": ["index", "fields"],
        "additionalProperties": false
    });
    let answer = Schema::compile(
        &wrap("items", json!({"type": "array", "items": item})),
        "config.schema",
    )?;
    let task = ctx.task(&format!("{TASK}\n\nFIELD SCHEMA\n{}", fields.value()));

    let mut all = Vec::with_capacity(inputs.len());
    for input in inputs {
        all.push(read_rows(input, ctx.common.max_input_bytes)?);
    }
    let batch = cfg.batch_rows as usize;
    ctx.plan(
        all.iter()
            .map(|rows| rows.len().div_ceil(batch) as u64)
            .sum(),
    )?;

    let mut out = Lines::create(ctx.path("rows.jsonl")?)?;
    for rows in &mut all {
        for start in (0..rows.len()).step_by(batch) {
            let end = (start + batch).min(rows.len());
            let indexed: Vec<(usize, &Map<String, Value>)> =
                (start..end).map(|i| (i, &rows[i])).collect();
            let expected: Vec<usize> = (start..end).collect();
            let text = rows_document(&indexed);
            let doc = Document {
                label: "rows",
                media_type: "application/x-ndjson",
                part: None,
                text: &text,
            };
            let check = |v: &Value| by_index(v, "items", &expected).map(|_| ());
            let value = ctx.json(&task, &[doc], &answer, &check).await?;
            let items = by_index(&value, "items", &expected)
                .map_err(|reason| LlmError::OutputInvalid { reason })?;
            for (index, item) in items {
                if let Value::Object(add) = &item["fields"] {
                    let row = &mut rows[index];
                    for (k, v) in add {
                        if cfg.overwrite || !row.contains_key(k) {
                            row.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            ctx.step();
        }
        for row in rows.iter() {
            out.push(&Value::Object(row.clone()))?;
        }
    }
    let count = out.count();
    let path = out.finish()?;
    let mut meta = ctx.lineage();
    meta.insert("row_count".into(), json!(count));
    ctx.out
        .output("rows", &path, "application/x-ndjson", Value::Object(meta));
    Ok(())
}
