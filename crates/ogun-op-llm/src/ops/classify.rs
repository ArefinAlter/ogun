//! `llm.classify`: labels from a closed set, per artifact (by its first chunk) or per row.

use std::collections::{BTreeMap, BTreeSet};

use ogun_llm::prompt::Document;
use ogun_llm::schema::wrap;
use ogun_llm::{LlmError, Schema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{by_index, rows_document};
use crate::common::{self, Ctx, Lines, counts, head, read_rows, read_text};
use crate::protocol::Input;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Unit {
    Artifact,
    Row,
}

fn default_unit() -> Unit {
    Unit::Artifact
}

fn default_batch() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// The closed label set, 2 to 256 distinct strings.
    labels: Vec<String>,
    /// Several labels per item instead of exactly one.
    #[serde(default)]
    multi_label: bool,
    /// What is classified.
    #[serde(default = "default_unit")]
    unit: Unit,
    /// Rows per call when `unit` is `row`.
    #[serde(default = "default_batch")]
    batch_rows: u32,
}

pub async fn run(ctx: &mut Ctx, own: Map<String, Value>, inputs: &[Input]) -> Result<(), LlmError> {
    let cfg: Config = common::own(own)?;
    let distinct: BTreeSet<&String> = cfg.labels.iter().collect();
    if !(2..=256).contains(&cfg.labels.len())
        || distinct.len() != cfg.labels.len()
        || cfg.labels.iter().any(|l| l.is_empty() || l.len() > 128)
    {
        return Err(LlmError::config(
            "config.labels",
            "2 to 256 distinct labels of 1 to 128 bytes",
        ));
    }
    if !(1..=200).contains(&cfg.batch_rows) {
        return Err(LlmError::config(
            "config.batch_rows",
            "an integer from 1 to 200",
        ));
    }
    let label = json!({"type": "string", "enum": cfg.labels});
    let (member, value_schema) = if cfg.multi_label {
        ("labels", json!({"type": "array", "items": label}))
    } else {
        ("label", label)
    };
    let how = if cfg.multi_label {
        "every label that applies (possibly none)"
    } else {
        "exactly one label, the best fit"
    };
    let label_list = cfg
        .labels
        .iter()
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut tally: BTreeMap<String, u64> = cfg.labels.iter().map(|l| (l.clone(), 0)).collect();
    let mut count = |v: &Value| {
        let mut bump = |s: &str| {
            if let Some(n) = tally.get_mut(s) {
                *n += 1;
            }
        };
        match v {
            Value::String(s) => bump(s),
            Value::Array(xs) => xs.iter().filter_map(Value::as_str).for_each(bump),
            _ => {}
        }
    };
    let mut out = Lines::create(ctx.path("labels.jsonl")?)?;

    match cfg.unit {
        Unit::Artifact => {
            let answer = Schema::compile(&wrap(member, value_schema), "config.labels")?;
            let task = ctx.task(&format!(
                "Classify the document. Choose {how} from this closed set and answer with a JSON object whose \
                 `{member}` member holds the choice; never use a label outside the set.\n\nLABELS\n{label_list}"
            ));
            ctx.plan(inputs.len() as u64)?;
            let chunk = ctx.common.chunk_bytes as usize;
            for input in inputs {
                let text = read_text(input, ctx.common.max_input_bytes)?;
                let doc = Document {
                    label: "1",
                    media_type: &input.media_type,
                    part: None,
                    text: head(&text, chunk),
                };
                let value = ctx.json(&task, &[doc], &answer, &|_| Ok(())).await?;
                count(&value[member]);
                out.push(
                    &json!({"blake3": input.blake3, "port": input.port, member: value[member]}),
                )?;
                ctx.step();
            }
        }
        Unit::Row => {
            let item = json!({
                "type": "object",
                "properties": { "index": { "type": "integer" }, member: value_schema },
                "required": ["index", member],
                "additionalProperties": false
            });
            let answer = Schema::compile(
                &wrap("items", json!({"type": "array", "items": item})),
                "config.labels",
            )?;
            let task = ctx.task(&format!(
                "Each document line is a JSON object with an `index` and a `row`. Classify every row: choose {how} \
                 from this closed set. Answer with a JSON object whose `items` member has exactly one entry per row, \
                 {{\"index\": <the row's index>, \"{member}\": <the choice>}}; never use a label outside the set.\
                 \n\nLABELS\n{label_list}"
            ));
            let mut all = Vec::with_capacity(inputs.len());
            for input in inputs {
                all.push(read_rows(input, ctx.common.max_input_bytes)?);
            }
            let batch = cfg.batch_rows as usize;
            ctx.plan(all.iter().map(|r| r.len().div_ceil(batch) as u64).sum())?;
            for (input, rows) in inputs.iter().zip(&all) {
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
                        count(&item[member]);
                        out.push(
                            &json!({"blake3": input.blake3, "index": index, member: item[member]}),
                        )?;
                    }
                    ctx.step();
                }
            }
        }
    }
    let lines = out.count();
    let path = out.finish()?;
    let mut meta = ctx.lineage();
    meta.insert("item_count".into(), json!(lines));
    meta.insert("label_counts".into(), counts(&tally));
    ctx.out
        .output("labels", &path, "application/x-ndjson", Value::Object(meta));
    Ok(())
}
