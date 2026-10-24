//! `llm.filter`: keeps or rejects each artifact by a natural-language criterion (judged on its first chunk).

use ogun_llm::prompt::Document;
use ogun_llm::{LlmError, Schema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::common::{self, Ctx, copy_input, extension_for, head, is_text, read_text};
use crate::protocol::Input;
use crate::sanitize;

/// Longest model-written reason kept in metadata.
const MAX_REASON_BYTES: usize = 500;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// The criterion an artifact must meet to be kept.
    criterion: String,
}

pub async fn run(ctx: &mut Ctx, own: Map<String, Value>, inputs: &[Input]) -> Result<(), LlmError> {
    let cfg: Config = common::own(own)?;
    if cfg.criterion.trim().is_empty() || cfg.criterion.len() > 4096 {
        return Err(LlmError::config(
            "config.criterion",
            "a non-empty string of at most 4 KiB",
        ));
    }
    let answer = Schema::compile(
        &json!({
            "type": "object",
            "properties": { "keep": { "type": "boolean" }, "reason": { "type": "string" } },
            "required": ["keep", "reason"],
            "additionalProperties": false
        }),
        "config",
    )?;
    let task = ctx.task(&format!(
        "Decide whether the document meets the criterion below. Answer with a JSON object: `keep` is true when it \
         meets the criterion and false otherwise, `reason` is one short sentence.\n\nCRITERION\n{}",
        cfg.criterion
    ));
    ctx.plan(inputs.len() as u64)?;
    let chunk = ctx.common.chunk_bytes as usize;
    for (n, input) in inputs.iter().enumerate() {
        let text = if is_text(&input.media_type) {
            read_text(input, ctx.common.max_input_bytes)?
        } else {
            return Err(LlmError::Input(format!(
                "llm.filter reads text; media type {} is not text",
                common::clean(&input.media_type)
            )));
        };
        let doc = Document {
            label: "1",
            media_type: &input.media_type,
            part: None,
            text: head(&text, chunk),
        };
        let value = ctx.json(&task, &[doc], &answer, &|_| Ok(())).await?;
        let keep = value["keep"].as_bool().unwrap_or(false);
        let mut reason = value["reason"].as_str().unwrap_or_default().to_owned();
        if reason.len() > MAX_REASON_BYTES {
            let mut end = MAX_REASON_BYTES;
            while !reason.is_char_boundary(end) {
                end -= 1;
            }
            reason.truncate(end);
        }
        let (port, dir) = if keep {
            ("artifacts", "kept")
        } else {
            ("rejected", "rejected")
        };
        let name = input.display_name();
        let ext = sanitize::extension(&name)
            .unwrap_or_else(|| extension_for(&input.media_type).to_owned());
        let path = ctx.path(&format!("{dir}/{n:04}-{}.{ext}", input.short()))?;
        copy_input(input, &path)?;
        let mut meta = ctx.lineage();
        meta.insert("source_blake3".into(), json!(input.blake3));
        meta.insert("keep".into(), json!(keep));
        // Model-written text: marked untrusted for every consumer (agents, Studio).
        meta.insert("untrusted_reason".into(), json!(reason));
        if !name.is_empty()
            && let Some(m) = &input.metadata
            && let Some(f) = m.get("file_name")
        {
            meta.insert("file_name".into(), f.clone());
        }
        ctx.out
            .output(port, &path, &input.media_type, Value::Object(meta));
        ctx.step();
    }
    Ok(())
}
