//! `llm.map` (per-artifact transformation) and `llm.rename` (per-artifact file naming).

use std::collections::HashSet;

use ogun_llm::prompt::Document;
use ogun_llm::schema::wrap;
use ogun_llm::{LlmError, Schema};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::common::{self, Ctx, copy_input, extension_for, head, is_text, read_text, write_file};
use crate::protocol::Input;
use crate::sanitize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MapConfig {
    /// Media type of the results (a text type); the input's when absent.
    #[serde(default)]
    output_media_type: Option<String>,
}

const MAP_TASK: &str = "Transform the document as the workflow's instructions below say. Answer with a JSON object \
whose `content` member is the transformed text and nothing else. When the document is one part of a larger \
document, transform only that part; the parts are joined in order afterwards.";

pub async fn map(ctx: &mut Ctx, own: Map<String, Value>, inputs: &[Input]) -> Result<(), LlmError> {
    let cfg: MapConfig = common::own(own)?;
    if ctx.common.instructions.is_none() {
        return Err(LlmError::config(
            "config.instructions",
            "llm.map needs instructions describing the transformation",
        ));
    }
    if let Some(mt) = &cfg.output_media_type
        && !is_text(mt)
    {
        return Err(LlmError::config(
            "config.output_media_type",
            "a text media type",
        ));
    }
    let answer = Schema::compile(&wrap("content", json!({"type": "string"})), "config")?;
    let task = ctx.task(MAP_TASK);
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

    for (n, (input, parts)) in inputs.iter().zip(&plan).enumerate() {
        let mut result = String::new();
        let count = parts.len();
        for (i, text) in parts.iter().enumerate() {
            let doc = Document {
                label: "1",
                media_type: &input.media_type,
                part: (count > 1).then_some((i + 1, count)),
                text,
            };
            let value = ctx.json(&task, &[doc], &answer, &|_| Ok(())).await?;
            result.push_str(value["content"].as_str().unwrap_or_default());
            ctx.step();
        }
        let media_type = cfg
            .output_media_type
            .clone()
            .unwrap_or_else(|| input.media_type.clone());
        let path = ctx.path(&format!(
            "mapped/{n:04}-{}.{}",
            input.short(),
            extension_for(&media_type)
        ))?;
        write_file(&path, result.as_bytes())?;
        let mut meta = ctx.lineage();
        meta.insert("source_blake3".into(), json!(input.blake3));
        ctx.out
            .output("artifacts", &path, &media_type, Value::Object(meta));
    }
    Ok(())
}

fn default_max_name() -> u32 {
    100
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameConfig {
    /// Longest stem, in bytes.
    #[serde(default = "default_max_name")]
    max_name_bytes: u32,
    /// Keep the current extension (default) or derive one from the media type.
    #[serde(default = "keep_ext")]
    keep_extension: bool,
}

fn keep_ext() -> bool {
    true
}

const RENAME_TASK: &str = "Propose a short, descriptive file name for the content document. The current-name \
document holds the file's present name, which may be meaningless. Answer with a JSON object whose `file_name` member \
is the name: lowercase words separated by hyphens, no directory, no extension, at most eight words.";

pub async fn rename(
    ctx: &mut Ctx,
    own: Map<String, Value>,
    inputs: &[Input],
) -> Result<(), LlmError> {
    let cfg: RenameConfig = common::own(own)?;
    if !(16..=200).contains(&cfg.max_name_bytes) {
        return Err(LlmError::config(
            "config.max_name_bytes",
            "an integer from 16 to 200",
        ));
    }
    let answer = Schema::compile(&wrap("file_name", json!({"type": "string"})), "config")?;
    let task = ctx.task(RENAME_TASK);
    ctx.plan(inputs.len() as u64)?;
    let chunk = ctx.common.chunk_bytes as usize;
    let mut taken = HashSet::new();
    for input in inputs {
        let current = input.display_name();
        let text = if is_text(&input.media_type) {
            read_text(input, ctx.common.max_input_bytes)?
        } else {
            // Binary content: the model sees only the name and the media type.
            String::new()
        };
        let docs = [
            Document {
                label: "current-name",
                media_type: "text/plain",
                part: None,
                text: &current,
            },
            Document {
                label: "content",
                media_type: &input.media_type,
                part: None,
                text: head(&text, chunk),
            },
        ];
        let value = ctx.json(&task, &docs, &answer, &|_| Ok(())).await?;
        let proposed = value["file_name"].as_str().unwrap_or_default();
        let stem = sanitize::stem(proposed, cfg.max_name_bytes as usize)
            .or_else(|| sanitize::stem(&current, cfg.max_name_bytes as usize))
            .unwrap_or_else(|| format!("artifact-{}", input.short()));
        let ext = if cfg.keep_extension {
            sanitize::extension(&current)
        } else {
            None
        }
        .unwrap_or_else(|| extension_for(&input.media_type).to_owned());
        let name = sanitize::unique(&stem, Some(&ext), &mut taken);
        let path = ctx.path(&format!("renamed/{name}"))?;
        copy_input(input, &path)?;
        let mut meta = ctx.lineage();
        meta.insert("file_name".into(), json!(name));
        meta.insert("source_blake3".into(), json!(input.blake3));
        ctx.out
            .output("artifacts", &path, &input.media_type, Value::Object(meta));
        ctx.step();
    }
    Ok(())
}
