//! `jsonl-stdio/0` (O-03 section 4): the start line on stdin and the event lines on stdout.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

use ogun_llm::{KeyMap, LlmError, Usage};
use ogun_types::OgunError;
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

/// Largest accepted start line. It carries the configuration, the ladder, the input list and the secrets.
const MAX_START_BYTES: u64 = 16 * 1024 * 1024;

/// The start line.
#[derive(Debug, Deserialize)]
pub struct Start {
    /// Always `start`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The node instance, `ni_...` (diagnostics only).
    #[serde(default)]
    pub node_instance: Option<String>,
    /// The operator, `name` or `name@major`, when the command line does not name it.
    #[serde(default)]
    pub op: Option<String>,
    /// The node's configuration with `ladder` and `llm_budget_usd_micro` injected by the runner.
    #[serde(default)]
    pub config: Value,
    /// Input artifacts.
    #[serde(default)]
    pub inputs: Vec<Input>,
    /// Resolved values of the ladder's `api_key_secret` names.
    #[serde(default)]
    pub secrets: KeyMap,
    /// The last checkpoint. Model operators v0 write none and ignore it.
    #[serde(default)]
    pub checkpoint: Option<Value>,
}

/// One input artifact of the start line.
#[derive(Debug, Clone, Deserialize)]
pub struct Input {
    /// `node_id.output` it came through.
    pub port: String,
    /// Read-only path.
    pub path: PathBuf,
    /// Content digest.
    pub blake3: String,
    /// Media type.
    pub media_type: String,
    /// Size, when the runner passes it.
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// Metadata from the producing operator.
    #[serde(default)]
    pub metadata: Option<Value>,
}

impl Input {
    /// A file name for the artifact: `metadata.file_name`, `metadata.name`, else the stored path's name.
    #[must_use]
    pub fn display_name(&self) -> String {
        let meta = |k: &str| {
            self.metadata
                .as_ref()
                .and_then(|m| m.get(k))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        meta("file_name")
            .or_else(|| meta("name"))
            .or_else(|| {
                self.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default()
    }

    /// The first twelve hex digits of the digest, for file names.
    #[must_use]
    pub fn short(&self) -> &str {
        let end = self.blake3.len().min(12);
        &self.blake3[..end]
    }
}

/// Reads and parses the start line. The line buffer is zeroized when dropped (it holds the secrets).
pub fn read_start() -> Result<Start, LlmError> {
    let mut line = Zeroizing::new(String::with_capacity(64 * 1024));
    let stdin = std::io::stdin();
    let mut lock = stdin.lock().take(MAX_START_BYTES);
    lock.read_line(&mut line)
        .map_err(|_| LlmError::Input("the start line is not readable UTF-8".into()))?;
    if line.trim().is_empty() {
        return Err(LlmError::Input("no start line on stdin".into()));
    }
    let start: Start = serde_json::from_str(&line).map_err(|e| {
        LlmError::Input(format!(
            "the start line is malformed ({})",
            e.classify_name()
        ))
    })?;
    if start.kind != "start" {
        return Err(LlmError::Input("the first line is not a start line".into()));
    }
    Ok(start)
}

trait ClassifyName {
    fn classify_name(&self) -> &'static str;
}

impl ClassifyName for serde_json::Error {
    fn classify_name(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "shape",
            serde_json::error::Category::Eof => "truncated",
        }
    }
}

/// Writes event lines to stdout, one JSON object per line, flushed. A closed stdout is ignored: the runner
/// treats a missing `result` as a crash.
#[derive(Debug, Default, Clone, Copy)]
pub struct Emitter;

impl Emitter {
    fn line(self, v: &Value) {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = serde_json::to_writer(&mut out, v);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }

    /// `{"type":"progress","done","total"}`.
    pub fn progress(self, done: u64, total: u64) {
        self.line(&json!({"type": "progress", "done": done, "total": total}));
    }

    /// `{"type":"usage","provider","model","input_tokens","output_tokens","cost_microusd"}`.
    pub fn usage(self, u: &Usage) {
        self.line(&json!({
            "type": "usage",
            "provider": u.provider,
            "model": u.model,
            "input_tokens": u.input_tokens,
            "output_tokens": u.output_tokens,
            "cost_microusd": u.cost_microusd,
        }));
    }

    /// `{"type":"output","port","path","media_type","metadata"}`. `path` is absolute, inside scratch.
    pub fn output(self, port: &str, path: &Path, media_type: &str, metadata: Value) {
        self.line(&json!({
            "type": "output",
            "port": port,
            "path": path,
            "media_type": media_type,
            "metadata": metadata,
        }));
    }

    /// `{"type":"result","ok":true}`.
    pub fn result(self) {
        self.line(&json!({"type": "result", "ok": true}));
    }

    /// `{"type":"error","error":<envelope>}`.
    pub fn error(self, e: &OgunError) {
        self.line(&json!({"type": "error", "error": e}));
    }
}
