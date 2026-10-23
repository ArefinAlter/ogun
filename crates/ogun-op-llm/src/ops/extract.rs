//! `llm.extract`: rows matching a JSON Schema, from text, Markdown or JSON inputs; one JSON-lines output.

use ogun_llm::prompt::Document;
use ogun_llm::schema::wrap;
use ogun_llm::{LlmError, Schema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::common::{self, Ctx, Lines, read_text};
use crate::protocol::Input;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// JSON Schema of one row.
    schema: Value,
    /// When set, each row gets this member with `{"blake3", "part"}` of its source.
    #[serde(default)]
    source_field: Option<String>,
}

const TASK: &str = "Extract every record described by the row schema below from the documents. Answer with a JSON \
object whose `rows` member is an array of rows; every row must satisfy the row schema. Use only information present \
in the documents: leave a member out, or use null where the schema allows it, rather than guessing. When a document \
holds no such record, answer with an empty `rows` array. When a document is one part of a larger document, extract \
only the records in that part.";

pub async fn run(ctx: &mut Ctx, own: Map<String, Value>, inputs: &[Input]) -> Result<(), LlmError> {
    let cfg: Config = common::own(own)?;
    let row = Schema::compile(&cfg.schema, "config.schema")?;
    if let Some(f) = &cfg.source_field
        && (f.is_empty() || f.len() > 64)
    {
        return Err(LlmError::config("config.source_field", "1 to 64 bytes"));
    }
    let answer = Schema::compile(
        &wrap("rows", json!({"type": "array", "items": row.value()})),
        "config.schema",
    )?;
    let task = ctx.task(&format!("{TASK}\n\nROW SCHEMA\n{}", row.value()));

    let mut texts = Vec::with_capacity(inputs.len());
    for input in inputs {
        texts.push(read_text(input, ctx.common.max_input_bytes)?);
    }
    let chunk = ctx.common.chunk_bytes as usize;
    let plan: Vec<Vec<&str>> = texts
        .iter()
        .map(|t| common::nonempty_chunks(t, chunk))
        .collect();
    ctx.plan(plan.iter().map(|c| c.len() as u64).sum())?;

    let mut out = Lines::create(ctx.path("rows.jsonl")?)?;
    for (input, parts) in inputs.iter().zip(&plan) {
        let n = parts.len();
        for (i, text) in parts.iter().enumerate() {
            let doc = Document {
                label: "1",
                media_type: &input.media_type,
                part: (n > 1).then_some((i + 1, n)),
                text,
            };
            let value = ctx.json(&task, &[doc], &answer, &|_| Ok(())).await?;
            for mut r in value["rows"].as_array().cloned().unwrap_or_default() {
                if let (Some(field), Value::Object(m)) = (&cfg.source_field, &mut r) {
                    m.insert(
                        field.clone(),
                        json!({"blake3": input.blake3, "part": i + 1}),
                    );
                }
                out.push(&r)?;
            }
            ctx.step();
        }
    }
    let rows = out.count();
    let path = out.finish()?;
    let mut meta = ctx.lineage();
    meta.insert("row_count".into(), json!(rows));
    ctx.out
        .output("rows", &path, "application/x-ndjson", Value::Object(meta));
    Ok(())
}
