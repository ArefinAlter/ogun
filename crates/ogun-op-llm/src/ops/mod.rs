//! The model operators (O-03 section 8.5).

mod classify;
mod embed;
mod enrich;
mod extract;
mod filter;
mod infer;
mod transform;

use std::path::PathBuf;

use ogun_llm::{Capability, LlmError};
use serde_json::{Map, Value, json};

use crate::common::{
    self, COMMON_KEYS, Ctx, DEFAULT_CHUNK_BYTES, DEFAULT_EMBED_CHUNK_BYTES, EMBED_COMMON_KEYS,
};
use crate::protocol::Start;

/// The operators this binary runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `llm.extract`: rows matching a schema from text.
    Extract,
    /// `llm.enrich`: fields added to rows.
    Enrich,
    /// `llm.classify`: labels from a closed set.
    Classify,
    /// `llm.map`: per-artifact transformation.
    Map,
    /// `llm.rename`: per-artifact file naming.
    Rename,
    /// `llm.filter`: keep or reject artifacts by a criterion.
    Filter,
    /// `llm.schema`: a JSON Schema inferred from samples.
    Schema,
    /// `embedding.generate`: vectors.
    Embed,
}

impl Op {
    /// Parses `name` or `name@1`.
    pub fn parse(s: &str) -> Result<Self, LlmError> {
        let (name, major) = s.split_once('@').unwrap_or((s, "1"));
        if major != "1" {
            return Err(LlmError::Input(format!(
                "this build implements major version 1 of {}",
                common::clean(name)
            )));
        }
        Ok(match name {
            "llm.extract" => Self::Extract,
            "llm.enrich" => Self::Enrich,
            "llm.classify" => Self::Classify,
            "llm.map" => Self::Map,
            "llm.rename" => Self::Rename,
            "llm.filter" => Self::Filter,
            "llm.schema" => Self::Schema,
            "embedding.generate" => Self::Embed,
            other => {
                return Err(LlmError::Input(format!(
                    "ogun-op-llm does not implement {}",
                    common::clean(other)
                )));
            }
        })
    }

    /// The catalogue name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Extract => "llm.extract",
            Self::Enrich => "llm.enrich",
            Self::Classify => "llm.classify",
            Self::Map => "llm.map",
            Self::Rename => "llm.rename",
            Self::Filter => "llm.filter",
            Self::Schema => "llm.schema",
            Self::Embed => "embedding.generate",
        }
    }
}

/// Runs one operator to completion. Outputs are announced as they are written; the caller writes `result`
/// or `error`.
pub async fn run(op: Op, start: Start, scratch: PathBuf) -> Result<(), LlmError> {
    let (allowed, chunk, capability) = match op {
        Op::Embed => (
            EMBED_COMMON_KEYS,
            DEFAULT_EMBED_CHUNK_BYTES,
            Capability::Embeddings,
        ),
        _ => (COMMON_KEYS, DEFAULT_CHUNK_BYTES, Capability::Chat),
    };
    let (common, own) = common::split(start.config, allowed, chunk)?;
    let mut ctx = Ctx::new(common, capability, &start.secrets, scratch)?;
    let inputs = start.inputs;
    if inputs.is_empty() {
        return Err(LlmError::Input("the operator received no inputs".into()));
    }
    match op {
        Op::Extract => extract::run(&mut ctx, own, &inputs).await,
        Op::Enrich => enrich::run(&mut ctx, own, &inputs).await,
        Op::Classify => classify::run(&mut ctx, own, &inputs).await,
        Op::Map => transform::map(&mut ctx, own, &inputs).await,
        Op::Rename => transform::rename(&mut ctx, own, &inputs).await,
        Op::Filter => filter::run(&mut ctx, own, &inputs).await,
        Op::Schema => infer::run(&mut ctx, own, &inputs).await,
        Op::Embed => embed::run(&mut ctx, own, &inputs).await,
    }
}

/// A batch of rows as a JSON-lines document: `{"index": k, "row": {...}}` per line.
fn rows_document(rows: &[(usize, &Map<String, Value>)]) -> String {
    let mut text = String::new();
    for (index, row) in rows {
        text.push_str(&json!({"index": index, "row": row}).to_string());
        text.push('\n');
    }
    text
}

/// Checks that `answer[member]` is an array of objects whose `index` members are exactly `expected`, once
/// each, and returns them by index.
fn by_index(
    answer: &Value,
    member: &str,
    expected: &[usize],
) -> Result<Vec<(usize, Value)>, String> {
    let items = answer[member]
        .as_array()
        .ok_or_else(|| format!("`{member}` is not an array"))?;
    let mut seen = std::collections::BTreeMap::new();
    for item in items {
        let index = item["index"]
            .as_u64()
            .ok_or_else(|| "an item has no integer `index`".to_owned())?
            as usize;
        if !expected.contains(&index) {
            return Err(format!(
                "index {index} is not one of the rows in this batch"
            ));
        }
        if seen.insert(index, item.clone()).is_some() {
            return Err(format!("index {index} appears more than once"));
        }
    }
    let missing: Vec<String> = expected
        .iter()
        .filter(|i| !seen.contains_key(i))
        .map(ToString::to_string)
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "no item for the rows with index {}",
            missing.join(", ")
        ));
    }
    Ok(seen.into_iter().collect())
}
