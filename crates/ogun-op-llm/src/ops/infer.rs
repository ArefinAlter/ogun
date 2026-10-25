//! `llm.schema`: infers a JSON Schema from samples. The answer is checked locally: it must compile, and every
//! JSON sample must validate against it.

use ogun_llm::prompt::Document;
use ogun_llm::schema::wrap;
use ogun_llm::{LlmError, Schema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::common::{self, Ctx, head, is_json, is_jsonl, read_rows, read_text, write_file};
use crate::protocol::Input;

fn default_samples() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// Most samples shown to the model.
    #[serde(default = "default_samples")]
    max_samples: u32,
    /// `title` of the resulting schema.
    #[serde(default)]
    title: Option<String>,
}

const TASK: &str = "Infer a JSON Schema (draft 2020-12) that describes the sample records in the documents. Prefer \
precise types, mark members that every sample has as required, use enums only for small closed sets that are \
evident, and allow null where samples show it. Answer with a JSON object whose `schema_json` member is the schema \
serialised as a JSON string.";

pub async fn run(ctx: &mut Ctx, own: Map<String, Value>, inputs: &[Input]) -> Result<(), LlmError> {
    let cfg: Config = common::own(own)?;
    if !(1..=500).contains(&cfg.max_samples) {
        return Err(LlmError::config(
            "config.max_samples",
            "an integer from 1 to 500",
        ));
    }
    if let Some(t) = &cfg.title
        && (t.is_empty() || t.len() > 200)
    {
        return Err(LlmError::config("config.title", "1 to 200 bytes"));
    }
    let budget = ctx.common.chunk_bytes as usize;
    let mut samples: Vec<Value> = Vec::new();
    let mut texts: Vec<(String, String)> = Vec::new();
    for input in inputs {
        if samples.len() >= cfg.max_samples as usize {
            break;
        }
        if is_jsonl(&input.media_type) || is_json(&input.media_type) {
            for row in read_rows(input, ctx.common.max_input_bytes)? {
                if samples.len() >= cfg.max_samples as usize {
                    break;
                }
                samples.push(Value::Object(row));
            }
        } else {
            let text = read_text(input, ctx.common.max_input_bytes)?;
            texts.push((input.media_type.clone(), head(&text, budget).to_owned()));
        }
    }
    let mut sample_text = String::new();
    for s in &samples {
        let line = s.to_string();
        if sample_text.len() + line.len() + 1 > budget {
            break;
        }
        sample_text.push_str(&line);
        sample_text.push('\n');
    }
    let mut docs = Vec::new();
    if !sample_text.is_empty() {
        docs.push(Document {
            label: "samples",
            media_type: "application/x-ndjson",
            part: None,
            text: &sample_text,
        });
    }
    let labels: Vec<String> = (1..=texts.len()).map(|i| format!("text-{i}")).collect();
    for ((mt, text), label) in texts.iter().zip(&labels) {
        docs.push(Document {
            label,
            media_type: mt,
            part: None,
            text,
        });
    }
    if docs.is_empty() {
        return Err(LlmError::Input("no samples to infer a schema from".into()));
    }
    let answer = Schema::compile(&wrap("schema_json", json!({"type": "string"})), "config")?;
    let task = ctx.task(TASK);
    ctx.plan(1)?;
    let check = |v: &Value| -> Result<(), String> {
        let inferred = parse_schema(v)?;
        let compiled = Schema::compile(&inferred, "schema_json")
            .map_err(|_| "`schema_json` is not a usable JSON Schema".to_owned())?;
        for (i, s) in samples.iter().enumerate() {
            if let Err(p) = compiled.check(s) {
                return Err(format!("the schema rejects sample {} ({p})", i + 1));
            }
        }
        Ok(())
    };
    let value = ctx.json(&task, &docs, &answer, &check).await?;
    ctx.step();
    let mut inferred = parse_schema(&value).map_err(|reason| LlmError::OutputInvalid { reason })?;
    if let Value::Object(m) = &mut inferred {
        m.entry("$schema")
            .or_insert_with(|| json!("https://json-schema.org/draft/2020-12/schema"));
        if let Some(t) = &cfg.title {
            m.insert("title".into(), json!(t));
        }
    }
    let path = ctx.path("schema.json")?;
    let mut text = serde_json::to_string_pretty(&inferred)
        .map_err(|_| LlmError::Internal("cannot serialise the schema".into()))?;
    text.push('\n');
    write_file(&path, text.as_bytes())?;
    let mut meta = ctx.lineage();
    meta.insert("sample_count".into(), json!(samples.len() + texts.len()));
    meta.insert("samples_checked".into(), json!(samples.len()));
    ctx.out.output(
        "schema",
        &path,
        "application/schema+json",
        Value::Object(meta),
    );
    Ok(())
}

fn parse_schema(v: &Value) -> Result<Value, String> {
    let text = v["schema_json"]
        .as_str()
        .ok_or_else(|| "`schema_json` is not a string".to_owned())?;
    match serde_json::from_str::<Value>(text) {
        Ok(s @ Value::Object(_)) => Ok(s),
        Ok(_) => Err("`schema_json` is not a JSON object".to_owned()),
        Err(_) => Err("`schema_json` is not valid JSON".to_owned()),
    }
}
