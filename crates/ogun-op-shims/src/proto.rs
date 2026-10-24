//! The child side of `jsonl-stdio/0`: the start line, protocol lines, and running tools safely.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};
use zeroize::Zeroizing;

/// A failure reported as the `error` line.
#[derive(Debug, Clone)]
pub struct ShimError {
    /// Registered Ogun code.
    pub code: &'static str,
    /// Error class.
    pub class: &'static str,
    /// Message: never a configuration value, secret or URL query.
    pub message: String,
    /// Retry may help.
    pub retryable: bool,
}

impl ShimError {
    /// A failure.
    pub fn new(code: &'static str, class: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            class,
            message: message.into(),
            retryable: false,
        }
    }

    /// `op.config_invalid`.
    pub fn config(message: impl Into<String>) -> Self {
        Self::new("op.config_invalid", "invalid_request", message)
    }

    /// `op.input_invalid`.
    pub fn input(message: impl Into<String>) -> Self {
        Self::new("op.input_invalid", "invalid_request", message)
    }

    /// `op.io`, retryable.
    pub fn io(what: &str, e: &std::io::Error) -> Self {
        let mut err = Self::new("op.io", "disk", format!("{what}: {}", e.kind()));
        err.retryable = true;
        err
    }

    /// `op.tool_missing`.
    pub fn missing(tool: &str) -> Self {
        Self::new(
            "op.tool_missing",
            "unavailable",
            format!("{tool} is not installed"),
        )
    }

    /// `op.tool_failed`.
    pub fn failed(tool: &str, status: &std::process::ExitStatus) -> Self {
        Self::new(
            "op.tool_failed",
            "permanent",
            format!("{tool} exited unsuccessfully ({status}); see stderr"),
        )
    }
}

/// One input of the start line.
#[derive(Debug, Clone)]
pub struct Input {
    /// The producing node's id.
    pub port: String,
    /// Read-only path.
    pub path: PathBuf,
    /// Content digest.
    pub blake3: String,
    /// Media type.
    pub media_type: String,
    /// Metadata.
    pub metadata: Option<Value>,
}

impl Input {
    /// The best file name: `metadata.filename`, else the stored name.
    pub fn name(&self) -> String {
        self.metadata
            .as_ref()
            .and_then(|m| m.get("filename"))
            .and_then(Value::as_str)
            .map(sanitize)
            .unwrap_or_else(|| {
                self.path
                    .file_name()
                    .map_or_else(|| "input".to_owned(), |n| sanitize(&n.to_string_lossy()))
            })
    }

    /// The name without its extension.
    pub fn stem(&self) -> String {
        let name = self.name();
        Path::new(&name)
            .file_stem()
            .map_or(name.clone(), |s| s.to_string_lossy().into_owned())
    }
}

/// A file name with separators, control characters and leading dots removed.
pub fn sanitize(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() || c == ':' {
                '_'
            } else {
                c
            }
        })
        .collect();
    while s.starts_with('.') {
        s.remove(0);
    }
    s.truncate(200);
    if s.is_empty() { "file".to_owned() } else { s }
}

/// The parsed start line. Secret values are kept apart and zeroized on drop.
pub struct Shim {
    start: Value,
    secrets: BTreeMap<String, Zeroizing<String>>,
    /// The scratch directory (the working directory).
    pub scratch: PathBuf,
    /// Inputs.
    pub inputs: Vec<Input>,
}

impl Shim {
    /// Reads the start line from stdin.
    pub fn read() -> Result<Self, ShimError> {
        let mut line = Zeroizing::new(String::new());
        std::io::stdin()
            .lock()
            .take(64 << 20)
            .read_line(&mut line)
            .map_err(|e| ShimError::io("reading the start line", &e))?;
        let mut start: Value = serde_json::from_str(&line).map_err(|_| {
            ShimError::new(
                "op.protocol_violation",
                "permanent",
                "the start line is not JSON",
            )
        })?;
        if start.get("type").and_then(Value::as_str) != Some("start") {
            return Err(ShimError::new(
                "op.protocol_violation",
                "permanent",
                "the first line is not a start line",
            ));
        }
        let mut secrets = BTreeMap::new();
        if let Some(Value::Object(map)) = start.as_object_mut().and_then(|o| o.remove("secrets")) {
            for (k, v) in map {
                if let Value::String(s) = v {
                    secrets.insert(k, Zeroizing::new(s));
                }
            }
        }
        let scratch = start
            .get("scratch")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let inputs = start
            .get("inputs")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| {
                        Some(Input {
                            port: i.get("port")?.as_str()?.to_owned(),
                            path: PathBuf::from(i.get("path")?.as_str()?),
                            blake3: i
                                .get("blake3")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            media_type: i
                                .get("media_type")
                                .and_then(Value::as_str)
                                .unwrap_or("application/octet-stream")
                                .to_owned(),
                            metadata: i.get("metadata").cloned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            start,
            secrets,
            scratch,
            inputs,
        })
    }

    /// The node configuration.
    pub fn config(&self) -> &Map<String, Value> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        self.start
            .get("config")
            .and_then(Value::as_object)
            .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
    }

    /// A start line member.
    pub fn start(&self, key: &str) -> Option<&Value> {
        self.start.get(key)
    }

    /// An optional string member of the configuration.
    pub fn str(&self, key: &str) -> Result<Option<&str>, ShimError> {
        match self.config().get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(ShimError::config(format!("{key} must be a string"))),
        }
    }

    /// An optional integer member.
    pub fn u64(&self, key: &str) -> Result<Option<u64>, ShimError> {
        match self.config().get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .map(Some)
                .ok_or_else(|| ShimError::config(format!("{key} must be a non-negative integer"))),
        }
    }

    /// An optional boolean member.
    pub fn bool(&self, key: &str) -> Result<Option<bool>, ShimError> {
        match self.config().get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(ShimError::config(format!("{key} must be true or false"))),
        }
    }

    /// An optional list of strings.
    pub fn strings(&self, key: &str) -> Result<Vec<String>, ShimError> {
        match self.config().get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| ShimError::config(format!("{key} must be strings")))
                })
                .collect(),
            Some(_) => Err(ShimError::config(format!(
                "{key} must be a list of strings"
            ))),
        }
    }

    /// A configuration value that is a string or a secret reference.
    pub fn text_of(&self, v: &Value) -> Option<Zeroizing<String>> {
        match v {
            Value::String(s) => Some(Zeroizing::new(s.clone())),
            Value::Object(o) if o.len() == 1 => {
                let name = o.get("secret_ref")?.as_str()?;
                self.secrets
                    .get(name)
                    .map(|s| Zeroizing::new(s.as_str().to_owned()))
            }
            _ => None,
        }
    }

    /// The workspace, when granted.
    pub fn workspace(&self) -> Option<PathBuf> {
        self.start
            .get("workspace")
            .and_then(Value::as_str)
            .map(PathBuf::from)
    }

    /// A directory under scratch, created.
    pub fn dir(&self, name: &str) -> Result<PathBuf, ShimError> {
        let d = self.scratch.join(name);
        std::fs::create_dir_all(&d)
            .map_err(|e| ShimError::io("creating a scratch directory", &e))?;
        Ok(d)
    }

    /// Logs configuration members this shim does not use.
    pub fn note_unknown(&self, known: &[&str]) {
        for k in self.config().keys() {
            if !known.contains(&k.as_str()) && k != "ladder" {
                log(&format!(
                    "the configuration member {k} is not used by this operator"
                ));
            }
        }
    }
}

fn line(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, v);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// An `output` line.
pub fn output(port: &str, path: &Path, media_type: &str, metadata: Value) {
    let mut v = json!({"type": "output", "port": port, "path": path, "media_type": media_type});
    if !metadata.is_null() {
        v["metadata"] = metadata;
    }
    line(&v);
}

/// A `progress` line.
pub fn progress(done: u64, total: u64) {
    line(&json!({"type": "progress", "done": done, "total": total}));
}

/// A `log` line.
pub fn log(message: &str) {
    line(&json!({"type": "log", "message": message}));
}

/// The final `result` line.
pub fn ok() {
    line(&json!({"type": "result", "ok": true}));
}

/// The final `error` line.
pub fn fail(e: &ShimError) {
    line(
        &json!({"type": "error", "error": {"code": e.code, "class": e.class, "message": e.message, "retryable": e.retryable}}),
    );
}

/// Finds a tool on `PATH`.
pub fn find(names: &[&str]) -> Result<PathBuf, ShimError> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in names {
        for dir in std::env::split_paths(&path) {
            let c = dir.join(name);
            if c.is_file() {
                return Ok(c);
            }
        }
    }
    Err(ShimError::missing(
        names.first().copied().unwrap_or("a tool"),
    ))
}

/// Runs a tool with its stdout sent to stderr (stdout is the protocol stream).
pub fn run(cmd: &mut Command, tool: &str) -> Result<(), ShimError> {
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ShimError::missing(tool),
            _ => ShimError::io("starting a tool", &e),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(ShimError::failed(tool, &status))
    }
}

/// Runs a tool and returns its stdout (at most 256 MiB).
pub fn run_capture(cmd: &mut Command, tool: &str) -> Result<Vec<u8>, ShimError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ShimError::missing(tool),
            _ => ShimError::io("starting a tool", &e),
        })?;
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(256 << 20)
            .read_to_end(&mut out)
            .map_err(|e| ShimError::io("reading a tool's output", &e))?;
    }
    let status = child
        .wait()
        .map_err(|e| ShimError::io("waiting for a tool", &e))?;
    if status.success() {
        Ok(out)
    } else {
        Err(ShimError::failed(tool, &status))
    }
}

/// Runs a tool, writing `stdin` to it, stdout to stderr.
pub fn run_with_stdin(cmd: &mut Command, tool: &str, stdin: &[u8]) -> Result<(), ShimError> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ShimError::missing(tool),
            _ => ShimError::io("starting a tool", &e),
        })?;
    if let Some(mut w) = child.stdin.take() {
        w.write_all(stdin)
            .map_err(|e| ShimError::io("writing to a tool", &e))?;
    }
    let status = child
        .wait()
        .map_err(|e| ShimError::io("waiting for a tool", &e))?;
    if status.success() {
        Ok(())
    } else {
        Err(ShimError::failed(tool, &status))
    }
}

/// Regular files under `dir`, sorted, skipping hidden entries.
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => out.push(e.path()),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// Emits every file under `dir` on `port`.
pub fn emit_dir(dir: &Path, port: &str) -> usize {
    let files = files_under(dir);
    for f in &files {
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rel = f
            .strip_prefix(dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        output(
            port,
            f,
            media_type_for(&name),
            json!({"filename": name, "path": rel}),
        );
    }
    files.len()
}

/// A media type from a file name.
pub fn media_type_for(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    let table: &[(&str, &str)] = &[
        (".jsonl", "application/x-ndjson"),
        (".ndjson", "application/x-ndjson"),
        (".json", "application/json"),
        (".csv", "text/csv"),
        (".parquet", "application/vnd.apache.parquet"),
        (".txt", "text/plain"),
        (".md", "text/markdown"),
        (".html", "text/html"),
        (".pdf", "application/pdf"),
        (
            ".docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (".epub", "application/epub+zip"),
        (".tar", "application/x-tar"),
        (".zip", "application/zip"),
        (".gz", "application/gzip"),
        (".png", "image/png"),
        (".jpg", "image/jpeg"),
        (".mp3", "audio/mpeg"),
        (".wav", "audio/wav"),
        (".flac", "audio/flac"),
        (".ogg", "audio/ogg"),
        (".opus", "audio/opus"),
        (".m4a", "audio/mp4"),
        (".mp4", "video/mp4"),
        (".webm", "video/webm"),
        (".mkv", "video/x-matroska"),
        (".duckdb", "application/vnd.duckdb"),
        (".tex", "application/x-tex"),
        (".odt", "application/vnd.oasis.opendocument.text"),
        (".rst", "text/x-rst"),
    ];
    table
        .iter()
        .find(|(e, _)| n.ends_with(e))
        .map_or("application/octet-stream", |(_, m)| m)
}

/// A plain relative path (no absolute, `..`, `.`, backslash or NUL).
pub fn safe_rel(rel: &str) -> Result<PathBuf, ShimError> {
    if rel.is_empty()
        || rel.starts_with('/')
        || rel.contains('\\')
        || rel.contains('\0')
        || rel.contains(':')
    {
        return Err(ShimError::config(
            "a path must be relative to the workspace",
        ));
    }
    let mut out = PathBuf::new();
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        if seg == "." || seg == ".." {
            return Err(ShimError::config("a path must not contain . or .."));
        }
        out.push(seg);
    }
    Ok(out)
}

/// Standard base64 (RFC 4648), for an HTTP Basic credential.
pub fn base64(input: &[u8]) -> Zeroizing<String> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Zeroizing::new(String::with_capacity(input.len().div_ceil(3) * 4));
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(char::from(T[(n >> 18) as usize & 63]));
        out.push(char::from(T[(n >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 {
            char::from(T[(n >> 6) as usize & 63])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(T[n as usize & 63])
        } else {
            '='
        });
    }
    out
}
