//! What every model operator shares: the common configuration members, the run context (ladder, budget,
//! progress, usage lines), input reading, chunking and output files.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use ogun_llm::prompt::{self, Document};
use ogun_llm::{
    CallBudget, Capability, ChatRequest, EmbedRequest, EmbedResponse, Ladder, LadderConfig,
    LlmError, Message, Schema, Usage,
};
use serde_json::{Map, Value, json};

use crate::protocol::{Emitter, Input};

/// Default `max_calls`.
pub const DEFAULT_MAX_CALLS: u64 = 100;
/// Default `chunk_bytes` for chat operators.
pub const DEFAULT_CHUNK_BYTES: u64 = 32 * 1024;
/// Default `chunk_bytes` for `embedding.generate`.
pub const DEFAULT_EMBED_CHUNK_BYTES: u64 = 8 * 1024;
/// Default `max_input_bytes`.
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Every common member name.
pub const COMMON_KEYS: &[&str] = &[
    "ladder",
    "llm_budget_usd_micro",
    "providers",
    "max_calls",
    "max_output_tokens",
    "temperature_milli",
    "chunk_bytes",
    "max_input_bytes",
    "repair",
    "fallback",
    "instructions",
];

/// The common members `embedding.generate` takes.
pub const EMBED_COMMON_KEYS: &[&str] = &[
    "ladder",
    "llm_budget_usd_micro",
    "providers",
    "max_calls",
    "chunk_bytes",
    "max_input_bytes",
    "fallback",
];

/// The common configuration (O-03 section 8.4).
#[derive(Debug, Clone)]
pub struct Common {
    /// The ladder the runner injected.
    pub ladder: LadderConfig,
    /// The attempt's model budget the runner injected; `0` when absent (only free providers can run).
    pub budget_usd_micro: u64,
    /// Providers the node may use, in ladder order.
    pub providers: Option<Vec<String>>,
    /// Bound on provider requests (including fallbacks and repairs).
    pub max_calls: u64,
    /// Per-call output limit, capped by each provider's own.
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature in thousandths.
    pub temperature_milli: Option<u32>,
    /// Chunk size for large inputs.
    pub chunk_bytes: u64,
    /// Largest input artifact read.
    pub max_input_bytes: u64,
    /// One repair request for an invalid structured answer.
    pub repair: bool,
    /// Fall back down the ladder on transient failures.
    pub fallback: bool,
    /// Extra task instructions from the approved configuration.
    pub instructions: Option<String>,
}

fn int(map: &Map<String, Value>, key: &str, min: u64, max: u64) -> Result<Option<u64>, LlmError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(LlmError::config(
                format!("config.{key}"),
                format!("an integer from {min} to {max}"),
            )),
        },
    }
}

fn boolean(map: &Map<String, Value>, key: &str, default: bool) -> Result<bool, LlmError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(LlmError::config(format!("config.{key}"), "true or false")),
    }
}

/// Splits a node configuration into the common members (`allowed` of them) and the operator's own, which
/// are returned for the operator to deserialise strictly.
pub fn split(
    config: Value,
    allowed: &[&str],
    default_chunk: u64,
) -> Result<(Common, Map<String, Value>), LlmError> {
    let Value::Object(mut map) = config else {
        return Err(LlmError::config("config", "must be an object"));
    };
    for key in COMMON_KEYS {
        if map.contains_key(*key) && !allowed.contains(key) {
            return Err(LlmError::config(
                format!("config.{key}"),
                "this operator does not take this member",
            ));
        }
    }
    let ladder = match map.get("ladder") {
        Some(v) => LadderConfig::from_value(v)?,
        None => {
            return Err(LlmError::NotConfigured(
                "the runner passed no ladder (config.ladder); is llm.json configured?".into(),
            ));
        }
    };
    let providers = match map.get("providers") {
        None | Some(Value::Null) => None,
        Some(Value::Array(xs)) if (1..=32).contains(&xs.len()) => Some(
            xs.iter()
                .map(|x| {
                    x.as_str().map(str::to_owned).ok_or_else(|| {
                        LlmError::config("config.providers", "an array of provider names")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(_) => {
            return Err(LlmError::config(
                "config.providers",
                "an array of 1 to 32 provider names",
            ));
        }
    };
    let instructions = match map.get("instructions") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() && s.len() <= 16 * 1024 => Some(s.clone()),
        Some(_) => {
            return Err(LlmError::config(
                "config.instructions",
                "a non-empty string of at most 16 KiB",
            ));
        }
    };
    let common = Common {
        budget_usd_micro: int(&map, "llm_budget_usd_micro", 0, u64::MAX)?.unwrap_or(0),
        providers,
        max_calls: int(&map, "max_calls", 1, 100_000)?.unwrap_or(DEFAULT_MAX_CALLS),
        max_output_tokens: int(&map, "max_output_tokens", 1, 1_000_000)?.map(|n| n as u32),
        temperature_milli: int(&map, "temperature_milli", 0, 2_000)?.map(|n| n as u32),
        chunk_bytes: int(&map, "chunk_bytes", 1_024, 1_048_576)?.unwrap_or(default_chunk),
        max_input_bytes: int(&map, "max_input_bytes", 1, 1 << 30)?
            .unwrap_or(DEFAULT_MAX_INPUT_BYTES),
        repair: boolean(&map, "repair", true)?,
        fallback: boolean(&map, "fallback", true)?,
        instructions,
        ladder,
    };
    for key in COMMON_KEYS {
        map.remove(*key);
    }
    Ok((common, map))
}

/// Deserialises an operator's own members strictly.
pub fn own<T: serde::de::DeserializeOwned>(map: Map<String, Value>) -> Result<T, LlmError> {
    serde_json::from_value(Value::Object(map))
        .map_err(|e| LlmError::config("config", config_problem(&e)))
}

fn config_problem(e: &serde_json::Error) -> String {
    let msg = e.to_string();
    let msg = msg.split(" at line ").next().unwrap_or("invalid");
    if msg.starts_with("unknown field") || msg.starts_with("missing field") {
        msg.split(", expected").next().unwrap_or(msg).to_owned()
    } else {
        match msg.rsplit_once(", expected ") {
            Some((_, exp)) => format!("invalid value, expected {exp}"),
            None => "invalid value".to_owned(),
        }
    }
}

/// The run context of one attempt.
pub struct Ctx {
    /// Configuration shared by all operators.
    pub common: Common,
    /// The usable rungs.
    pub ladder: Ladder,
    /// Money and call limits.
    pub budget: CallBudget,
    /// Event lines.
    pub out: Emitter,
    /// Working directory (scratch).
    pub scratch: PathBuf,
    /// `provider/model` of every call that answered.
    pub models: BTreeSet<String>,
    /// BLAKE3 of every system prompt used.
    pub prompts: BTreeSet<String>,
    done: u64,
    total: u64,
}

impl Ctx {
    /// Builds the ladder for `capability` from the injected configuration and the start line's secrets.
    pub fn new(
        common: Common,
        capability: Capability,
        keys: &ogun_llm::KeyMap,
        scratch: PathBuf,
    ) -> Result<Self, LlmError> {
        let ladder = Ladder::build(
            &common.ladder,
            keys,
            capability,
            common.providers.as_deref(),
        )?
        .with_fallback(common.fallback)
        .with_repair(common.repair);
        let budget = CallBudget::new(common.budget_usd_micro, common.max_calls);
        Ok(Self {
            common,
            ladder,
            budget,
            out: Emitter,
            scratch,
            models: BTreeSet::new(),
            prompts: BTreeSet::new(),
            done: 0,
            total: 0,
        })
    }

    /// Announces the number of planned calls. More than `max_calls` fails before any money is spent.
    pub fn plan(&mut self, total: u64) -> Result<(), LlmError> {
        if total > self.common.max_calls {
            return Err(LlmError::CallLimit {
                max_calls: self.common.max_calls,
            });
        }
        self.total = total;
        self.out.progress(0, total);
        Ok(())
    }

    /// One planned unit is done.
    pub fn step(&mut self) {
        self.done += 1;
        self.out.progress(self.done, self.total.max(self.done));
    }

    /// The task text with the node's `instructions` appended.
    #[must_use]
    pub fn task(&self, base: &str) -> String {
        match &self.common.instructions {
            Some(extra) => format!("{base}\n\nADDITIONAL INSTRUCTIONS FROM THE WORKFLOW\n{extra}"),
            None => base.to_owned(),
        }
    }

    /// One structured call over `docs`.
    pub async fn json(
        &mut self,
        task: &str,
        docs: &[Document<'_>],
        schema: &Schema,
        check: &dyn Fn(&Value) -> Result<(), String>,
    ) -> Result<Value, LlmError> {
        let p = prompt::build(task, docs);
        self.prompts.insert(p.system_blake3());
        let req = ChatRequest {
            system: p.system,
            messages: vec![Message::user(p.user)],
            schema: None,
            max_output_tokens: self.common.max_output_tokens,
            temperature_milli: self.common.temperature_milli,
        };
        let Self {
            ladder,
            budget,
            out,
            models,
            ..
        } = self;
        let mut on_usage = |u: &Usage| {
            out.usage(u);
            models.insert(format!("{}/{}", u.provider, u.model));
        };
        let answer = ladder
            .chat_json(&req, schema, check, budget, &mut on_usage)
            .await?;
        Ok(answer.value)
    }

    /// One embeddings call.
    pub async fn embed(
        &mut self,
        inputs: Vec<String>,
        dimensions: Option<u32>,
        pinned: &mut Option<usize>,
    ) -> Result<EmbedResponse, LlmError> {
        let req = EmbedRequest { inputs, dimensions };
        let Self {
            ladder,
            budget,
            out,
            models,
            ..
        } = self;
        let mut on_usage = |u: &Usage| {
            out.usage(u);
            models.insert(format!("{}/{}", u.provider, u.model));
        };
        ladder.embed(&req, pinned, budget, &mut on_usage).await
    }

    /// Lineage metadata every output carries (integers and strings only; O-02 has no floats).
    #[must_use]
    pub fn lineage(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("llm_calls".into(), json!(self.budget.calls()));
        m.insert(
            "llm_cost_usd_micro".into(),
            json!(self.budget.spent_usd_micro()),
        );
        m.insert("models".into(), json!(self.models));
        m.insert("prompt_blake3".into(), json!(self.prompts));
        m
    }

    /// A path inside scratch, creating its directory.
    pub fn path(&self, rel: &str) -> Result<PathBuf, LlmError> {
        let p = self.scratch.join(rel);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| io("cannot create a scratch directory", &e))?;
        }
        Ok(p)
    }
}

/// An I/O failure as an error that names no path.
#[must_use]
pub fn io(what: &str, e: &std::io::Error) -> LlmError {
    LlmError::Internal(format!("{what}: {}", e.kind()))
}

/// Whether the operator can read the media type as text.
#[must_use]
pub fn is_text(media_type: &str) -> bool {
    let mt = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mt.starts_with("text/")
        || mt.ends_with("+json")
        || mt.ends_with("+xml")
        || matches!(
            mt.as_str(),
            "application/json"
                | "application/x-ndjson"
                | "application/jsonl"
                | "application/ndjson"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/csv"
                | "application/javascript"
                | "application/toml"
        )
}

/// Whether the media type holds JSON lines.
#[must_use]
pub fn is_jsonl(media_type: &str) -> bool {
    let mt = media_type.split(';').next().unwrap_or("").trim();
    matches!(
        mt,
        "application/x-ndjson"
            | "application/jsonl"
            | "application/ndjson"
            | "application/x-jsonlines"
    )
}

/// Whether the media type holds one JSON document.
#[must_use]
pub fn is_json(media_type: &str) -> bool {
    let mt = media_type.split(';').next().unwrap_or("").trim();
    mt == "application/json" || mt.ends_with("+json")
}

fn read_bytes(input: &Input, max: u64) -> Result<Vec<u8>, LlmError> {
    if input.size_bytes.is_some_and(|n| n > max) {
        return Err(LlmError::Input(format!(
            "an input is larger than max_input_bytes ({max})"
        )));
    }
    let meta = std::fs::metadata(&input.path).map_err(|e| io("cannot stat an input", &e))?;
    if meta.len() > max {
        return Err(LlmError::Input(format!(
            "an input is larger than max_input_bytes ({max})"
        )));
    }
    std::fs::read(&input.path).map_err(|e| io("cannot read an input", &e))
}

/// Reads a text input (invalid UTF-8 is replaced, not rejected).
pub fn read_text(input: &Input, max: u64) -> Result<String, LlmError> {
    if !is_text(&input.media_type) {
        return Err(LlmError::Input(format!(
            "media type {} is not text; extract text first (for example with pdf.extract)",
            clean(&input.media_type)
        )));
    }
    let bytes = read_bytes(input, max)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads rows: JSON lines, or a JSON array of objects, or one JSON object.
pub fn read_rows(input: &Input, max: u64) -> Result<Vec<Map<String, Value>>, LlmError> {
    let text = read_text(input, max)?;
    let as_rows = |values: Vec<Value>| {
        values
            .into_iter()
            .map(|v| match v {
                Value::Object(m) => Ok(m),
                _ => Err(LlmError::Input("every row must be a JSON object".into())),
            })
            .collect::<Result<Vec<_>, _>>()
    };
    if is_jsonl(&input.media_type) || !is_json(&input.media_type) {
        let mut values = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            values.push(serde_json::from_str(line).map_err(|_| {
                LlmError::Input(format!("line {} of a JSON-lines input is not JSON", i + 1))
            })?);
        }
        return as_rows(values);
    }
    match serde_json::from_str(&text) {
        Ok(Value::Array(xs)) => as_rows(xs),
        Ok(Value::Object(m)) => Ok(vec![m]),
        _ => Err(LlmError::Input(
            "a JSON input must be an array of objects or one object".into(),
        )),
    }
}

/// Splits text into chunks of at most `max` bytes, preferring paragraph, then line, then character
/// boundaries.
#[must_use]
pub fn chunks(text: &str, max: usize) -> Vec<&str> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let window = &rest[..cut];
        let at = window
            .rfind("\n\n")
            .map(|i| i + 2)
            .filter(|&i| i > max / 4)
            .or_else(|| window.rfind('\n').map(|i| i + 1).filter(|&i| i > max / 4))
            .unwrap_or(cut);
        // A limit smaller than one character still makes progress by one character.
        let at = if at == 0 {
            rest.char_indices().nth(1).map_or(rest.len(), |(i, _)| i)
        } else {
            at
        };
        out.push(&rest[..at]);
        rest = &rest[at..];
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest);
    }
    out
}

/// [`chunks`] without whitespace-only chunks (an empty input needs no model call).
#[must_use]
pub fn nonempty_chunks(text: &str, max: usize) -> Vec<&str> {
    chunks(text, max)
        .into_iter()
        .filter(|c| !c.trim().is_empty())
        .collect()
}

/// The first chunk of a text, cut at a boundary.
#[must_use]
pub fn head(text: &str, max: usize) -> &str {
    chunks(text, max).into_iter().next().unwrap_or("")
}

/// A media type reduced to safe characters for messages.
#[must_use]
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '+' | '-' | '_'))
        .take(100)
        .collect()
}

/// A JSON-lines file writer inside scratch.
pub struct Lines {
    path: PathBuf,
    file: BufWriter<std::fs::File>,
    count: u64,
}

impl Lines {
    /// Creates the file.
    pub fn create(path: PathBuf) -> Result<Self, LlmError> {
        let file = std::fs::File::create(&path).map_err(|e| io("cannot create an output", &e))?;
        Ok(Self {
            path,
            file: BufWriter::new(file),
            count: 0,
        })
    }

    /// Writes one value as a line.
    pub fn push(&mut self, v: &Value) -> Result<(), LlmError> {
        serde_json::to_writer(&mut self.file, v)
            .map_err(|_| LlmError::Internal("cannot write an output line".into()))?;
        self.file
            .write_all(b"\n")
            .map_err(|e| io("cannot write an output", &e))?;
        self.count += 1;
        Ok(())
    }

    /// Lines written.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Flushes and syncs; returns the path.
    pub fn finish(self) -> Result<PathBuf, LlmError> {
        let file = self
            .file
            .into_inner()
            .map_err(|e| io("cannot flush an output", e.error()))?;
        file.sync_all()
            .map_err(|e| io("cannot sync an output", &e))?;
        Ok(self.path)
    }
}

/// Writes a whole file inside scratch.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), LlmError> {
    std::fs::write(path, bytes).map_err(|e| io("cannot write an output", &e))
}

/// Copies an input into scratch.
pub fn copy_input(input: &Input, to: &Path) -> Result<(), LlmError> {
    std::fs::copy(&input.path, to)
        .map(|_| ())
        .map_err(|e| io("cannot copy an input", &e))
}

/// A file extension for a media type.
#[must_use]
pub fn extension_for(media_type: &str) -> &'static str {
    match media_type.split(';').next().unwrap_or("").trim() {
        "text/markdown" => "md",
        "text/csv" | "application/csv" => "csv",
        "text/html" => "html",
        "application/json" => "json",
        "application/x-ndjson" | "application/jsonl" | "application/ndjson" => "jsonl",
        "application/xml" | "text/xml" => "xml",
        "application/yaml" | "application/x-yaml" | "text/yaml" => "yaml",
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        _ => "txt",
    }
}

/// Counts of label values, as integer metadata.
#[must_use]
pub fn counts(labels: &BTreeMap<String, u64>) -> Value {
    Value::Object(labels.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_the_text_and_respect_the_limit() {
        let text = "para one\n\npara two is longer\nwith lines\n\nthree\n".repeat(50);
        let parts = chunks(&text, 100);
        assert!(parts.iter().all(|p| p.len() <= 100));
        assert_eq!(parts.concat(), text);
        let uni = "é".repeat(100);
        let parts = chunks(&uni, 7);
        assert_eq!(parts.concat(), uni);
        assert_eq!(chunks("", 10), vec![""]);
    }
}
