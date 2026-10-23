//! `embedding.generate`: one vector per text chunk, or per row (`text_field`), as JSON lines.
//!
//! All vectors of one attempt come from one provider and model: after the first successful call the ladder
//! is pinned to that rung, because vectors of different models are not comparable.

use ogun_llm::LlmError;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::common::{self, Ctx, Lines, read_rows, read_text};
use crate::protocol::Input;

fn default_batch() -> u32 {
    32
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// Embed this string member of each row instead of chunks of text inputs.
    #[serde(default)]
    text_field: Option<String>,
    /// Texts per call.
    #[serde(default = "default_batch")]
    batch_size: u32,
    /// Vector size, where the provider supports choosing it.
    #[serde(default)]
    dimensions: Option<u32>,
}

/// One text to embed and where it came from.
struct Item {
    blake3: String,
    /// `("chunk", n)` or `("index", n)`.
    position: (&'static str, usize),
    text: String,
}

pub async fn run(ctx: &mut Ctx, own: Map<String, Value>, inputs: &[Input]) -> Result<(), LlmError> {
    let cfg: Config = common::own(own)?;
    if !(1..=512).contains(&cfg.batch_size) {
        return Err(LlmError::config(
            "config.batch_size",
            "an integer from 1 to 512",
        ));
    }
    if let Some(d) = cfg.dimensions
        && !(1..=65_536).contains(&d)
    {
        return Err(LlmError::config(
            "config.dimensions",
            "an integer from 1 to 65536",
        ));
    }
    let mut items = Vec::new();
    let mut skipped = 0u64;
    for input in inputs {
        match &cfg.text_field {
            Some(field) => {
                for (i, row) in read_rows(input, ctx.common.max_input_bytes)?
                    .iter()
                    .enumerate()
                {
                    match row.get(field).and_then(Value::as_str) {
                        Some(t) if !t.trim().is_empty() => items.push(Item {
                            blake3: input.blake3.clone(),
                            position: ("index", i),
                            text: common::head(t, ctx.common.chunk_bytes as usize).to_owned(),
                        }),
                        _ => skipped += 1,
                    }
                }
            }
            None => {
                let text = read_text(input, ctx.common.max_input_bytes)?;
                for (i, chunk) in common::chunks(&text, ctx.common.chunk_bytes as usize)
                    .into_iter()
                    .enumerate()
                {
                    if chunk.trim().is_empty() {
                        continue;
                    }
                    items.push(Item {
                        blake3: input.blake3.clone(),
                        position: ("chunk", i),
                        text: chunk.to_owned(),
                    });
                }
            }
        }
    }
    if items.is_empty() {
        return Err(LlmError::Input("nothing to embed".into()));
    }
    let batch = cfg.batch_size as usize;
    ctx.plan(items.len().div_ceil(batch) as u64)?;
    let mut out = Lines::create(ctx.path("vectors.jsonl")?)?;
    let mut pinned = None;
    let mut dims: Option<usize> = None;
    let mut source = (String::new(), String::new());
    for group in items.chunks(batch) {
        let texts: Vec<String> = group.iter().map(|i| i.text.clone()).collect();
        let resp = ctx.embed(texts, cfg.dimensions, &mut pinned).await?;
        source = (resp.usage.provider.clone(), resp.usage.model.clone());
        for (item, vector) in group.iter().zip(&resp.vectors) {
            if *dims.get_or_insert(vector.len()) != vector.len() {
                return Err(LlmError::Unavailable {
                    provider: resp.usage.provider.clone(),
                    reason: "vectors of different sizes".into(),
                });
            }
            out.push(&json!({
                "blake3": item.blake3,
                item.position.0: item.position.1,
                "provider": resp.usage.provider,
                "model": resp.usage.model,
                "dimensions": vector.len(),
                "vector": vector,
            }))?;
        }
        ctx.step();
    }
    let count = out.count();
    let path = out.finish()?;
    let mut meta = ctx.lineage();
    meta.insert("vector_count".into(), json!(count));
    meta.insert("dimensions".into(), json!(dims.unwrap_or(0)));
    meta.insert("provider".into(), json!(source.0));
    meta.insert("model".into(), json!(source.1));
    meta.insert("skipped_rows".into(), json!(skipped));
    ctx.out.output(
        "vectors",
        &path,
        "application/x-ndjson",
        Value::Object(meta),
    );
    Ok(())
}
