//! Model providers for the agent: streaming chat with tool calls (O-17 section 7, the standalone brief).
//!
//! Five wire families, all driven from one provider entry of the ladder (`llm.json`, O-03 section 8.1):
//!
//! | Kind | Endpoint | Streaming | Tool calls |
//! |---|---|---|---|
//! | `anthropic` | `POST {base}/v1/messages` | SSE `content_block_*` events | `tool_use` blocks with `input_json_delta` |
//! | `openai`, `openai_compatible` (Mistral, DeepSeek, Qwen, Kimi, GLM, vLLM, LM Studio, OpenRouter) | `POST {base}/chat/completions` | SSE `data:` chunks | `delta.tool_calls` fragments by index |
//! | `google` | `POST {base}/models/{model}:streamGenerateContent?alt=sse` | SSE `data:` chunks | `functionCall` parts |
//! | `ollama` | `POST {base}/api/chat` | NDJSON lines | `message.tool_calls` |
//!
//! A model that cannot call tools natively (a provider that answers 400 about `tools`) is driven with a JSON
//! protocol instead: the system prompt asks for `{"tool": ..., "arguments": {...}}` and the reply is parsed.
//!
//! Rules that hold for every kind: the key travels only in the provider's own auth header; no redirect is
//! followed; the request, the key, the URL query and the response text appear in no error and no log; a stream
//! is cancelled by dropping it; text and event sizes are bounded.

use std::time::Duration;

use futures::StreamExt;
use ogun_llm::config::{ProviderConfig, ProviderKind};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::watch;

use super::tools::{from_wire, wire_name};
use super::types::{Msg, ToolCall};

/// Most text one answer may hold.
const MAX_TEXT: usize = 512 * 1024;
/// Largest SSE event or NDJSON line.
const MAX_EVENT: usize = 4 * 1024 * 1024;
/// Attempts for 429 and 5xx before anything was received.
const ATTEMPTS: u32 = 3;

/// A tool the model may call.
#[derive(Debug, Clone)]
pub struct WireTool {
    /// The dotted name.
    pub name: String,
    /// What it does.
    pub description: String,
    /// JSON Schema of its arguments.
    pub schema: Value,
}

/// What one provider call needs.
pub struct ChatInput<'a> {
    /// The system prompt.
    pub system: &'a str,
    /// The conversation.
    pub msgs: &'a [Msg],
    /// The tools offered.
    pub tools: &'a [WireTool],
    /// The output limit.
    pub max_output: u32,
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// The model finished its turn.
    Stop,
    /// It asked for tool results.
    ToolUse,
    /// It ran out of output tokens.
    Length,
}

/// The finished answer.
#[derive(Debug, Clone)]
pub struct Completion {
    /// The words.
    pub text: String,
    /// The tools it called.
    pub calls: Vec<ToolCall>,
    /// Input tokens, as reported or estimated.
    pub input_tokens: u64,
    /// Output tokens, as reported or estimated.
    pub output_tokens: u64,
    /// Whether the provider reported the counts (otherwise they are estimated from bytes).
    pub reported: bool,
    /// How it ended.
    pub finish: Finish,
}

/// Why a call failed. Messages name the provider's error type, never its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// 401 or 403.
    Auth,
    /// The provider declined on policy grounds.
    Refused(String),
    /// 429, with the provider's own wait if it sent one.
    RateLimited(Option<u64>),
    /// 5xx, a timeout or a broken stream.
    Unavailable(String),
    /// 4xx other than the above.
    Rejected(String),
    /// The turn was stopped.
    Cancelled,
    /// A tool-less model: use the JSON protocol.
    NoTools,
}

impl ProviderError {
    /// A sentence for the thread.
    #[must_use]
    pub fn sentence(&self, provider: &str) -> String {
        match self {
            Self::Auth => {
                format!("{provider} rejected the API key. Check it in Settings, Providers.")
            }
            Self::Refused(why) => format!("{provider} declined to answer ({why})."),
            Self::RateLimited(_) => {
                format!("{provider} is rate limiting this key; try again in a moment.")
            }
            Self::Unavailable(why) => format!("{provider} could not be reached or failed ({why})."),
            Self::Rejected(why) => format!("{provider} rejected the request ({why})."),
            Self::Cancelled => "Stopped.".into(),
            Self::NoTools => format!("{provider} does not support tool calls."),
        }
    }
}

/// An identifier-looking provider error type (never free text).
fn ident(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    (!s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')))
    .then(|| s.to_owned())
}

fn client(cfg: &ProviderConfig) -> Result<reqwest::Client, ProviderError> {
    let timeout = Duration::from_millis(cfg.timeout_ms);
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        // Per-read inactivity, not a total, so a long stream is not cut off.
        .read_timeout(timeout)
        .user_agent(concat!("ogund/", env!("CARGO_PKG_VERSION")));
    if cfg.base().is_ok_and(|u| ogun_llm::config::is_loopback(&u)) {
        b = b.no_proxy();
    }
    b.build()
        .map_err(|_| ProviderError::Unavailable("cannot configure the HTTP client".into()))
}

fn estimate(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4)
}

fn msgs_bytes(input: &ChatInput<'_>) -> usize {
    input.system.len()
        + input
            .tools
            .iter()
            .map(|t| t.schema.to_string().len() + t.description.len())
            .sum::<usize>()
        + input
            .msgs
            .iter()
            .map(|m| match m {
                Msg::User { text } => text.len(),
                Msg::Assistant { text, tool_calls } => {
                    text.len()
                        + tool_calls
                            .iter()
                            .map(|c| c.args.to_string().len())
                            .sum::<usize>()
                }
                Msg::ToolResult { text, .. } => text.len(),
            })
            .sum::<usize>()
}

/// The worst-case input size in tokens, for the spend check before a call.
#[must_use]
pub fn input_bound_tokens(input: &ChatInput<'_>) -> u64 {
    (msgs_bytes(input) as u64).saturating_add(1024)
}

// ------------------------------------------------------------------------------------------- request bodies

fn openai_body(cfg: &ProviderConfig, i: &ChatInput<'_>, usage: bool) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": i.system })];
    for m in i.msgs {
        match m {
            Msg::User { text } => messages.push(json!({ "role": "user", "content": text })),
            Msg::Assistant { text, tool_calls } => {
                let mut v = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) } });
                if !tool_calls.is_empty() {
                    v["tool_calls"] = json!(tool_calls
                        .iter()
                        .map(|c| json!({ "id": c.id, "type": "function", "function": { "name": wire_name(&c.name), "arguments": c.args.to_string() } }))
                        .collect::<Vec<_>>());
                }
                messages.push(v);
            }
            Msg::ToolResult { call_id, text, .. } => {
                messages.push(json!({ "role": "tool", "tool_call_id": call_id, "content": text }))
            }
        }
    }
    let mut body = json!({ "model": cfg.model, "messages": messages, "stream": true });
    if usage {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if cfg.kind == ProviderKind::Openai {
        body["max_completion_tokens"] = json!(i.max_output);
    } else {
        body["max_tokens"] = json!(i.max_output);
    }
    if !i.tools.is_empty() {
        body["tools"] = json!(i
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "function": { "name": wire_name(&t.name), "description": t.description, "parameters": t.schema } }))
            .collect::<Vec<_>>());
    }
    body
}

fn anthropic_body(cfg: &ProviderConfig, i: &ChatInput<'_>) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    let flush = |messages: &mut Vec<Value>, pending: &mut Vec<Value>| {
        if !pending.is_empty() {
            messages.push(json!({ "role": "user", "content": std::mem::take(pending) }));
        }
    };
    for m in i.msgs {
        match m {
            Msg::ToolResult {
                call_id,
                text,
                is_error,
                ..
            } => {
                pending.push(json!({ "type": "tool_result", "tool_use_id": call_id, "content": text, "is_error": is_error }));
            }
            Msg::User { text } => {
                flush(&mut messages, &mut pending);
                messages.push(json!({ "role": "user", "content": text }));
            }
            Msg::Assistant { text, tool_calls } => {
                flush(&mut messages, &mut pending);
                let mut blocks: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for c in tool_calls {
                    blocks.push(json!({ "type": "tool_use", "id": c.id, "name": wire_name(&c.name), "input": c.args }));
                }
                messages.push(json!({ "role": "assistant", "content": blocks }));
            }
        }
    }
    flush(&mut messages, &mut pending);
    let mut body = json!({
        "model": cfg.model,
        "max_tokens": i.max_output,
        "system": i.system,
        "messages": messages,
        "stream": true,
    });
    if !i.tools.is_empty() {
        body["tools"] = json!(i
            .tools
            .iter()
            .map(|t| json!({ "name": wire_name(&t.name), "description": t.description, "input_schema": t.schema }))
            .collect::<Vec<_>>());
    }
    body
}

/// Gemini accepts a narrow schema subset: an object without declared properties is not allowed, so an
/// open object argument travels as JSON text and is parsed when the call comes back.
fn gemini_schema(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            let is_open_object =
                m.get("type") == Some(&json!("object")) && !m.contains_key("properties");
            for (k, x) in m {
                match (k.as_str(), is_open_object) {
                    ("type", true) => {
                        out.insert("type".into(), json!("string"));
                    }
                    ("additionalProperties" | "$schema", _) => {}
                    ("description", true) => {
                        out.insert(
                            "description".into(),
                            json!(format!("{} (as JSON text)", x.as_str().unwrap_or(""))),
                        );
                    }
                    _ => {
                        out.insert(k.clone(), gemini_schema(x));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(gemini_schema).collect()),
        other => other.clone(),
    }
}

fn gemini_body(i: &ChatInput<'_>) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    let flush = |contents: &mut Vec<Value>, pending: &mut Vec<Value>| {
        if !pending.is_empty() {
            contents.push(json!({ "role": "user", "parts": std::mem::take(pending) }));
        }
    };
    for m in i.msgs {
        match m {
            Msg::ToolResult { name, text, .. } => {
                pending.push(json!({ "functionResponse": { "name": wire_name(name), "response": { "result": text } } }));
            }
            Msg::User { text } => {
                flush(&mut contents, &mut pending);
                contents.push(json!({ "role": "user", "parts": [{ "text": text }] }));
            }
            Msg::Assistant { text, tool_calls } => {
                flush(&mut contents, &mut pending);
                let mut parts: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
                for c in tool_calls {
                    parts.push(
                        json!({ "functionCall": { "name": wire_name(&c.name), "args": c.args } }),
                    );
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
        }
    }
    flush(&mut contents, &mut pending);
    let mut body = json!({
        "systemInstruction": { "parts": [{ "text": i.system }] },
        "contents": contents,
        "generationConfig": { "maxOutputTokens": i.max_output },
    });
    if !i.tools.is_empty() {
        body["tools"] = json!([{ "functionDeclarations": i.tools.iter().map(|t| json!({
            "name": wire_name(&t.name), "description": t.description, "parameters": gemini_schema(&t.schema)
        })).collect::<Vec<_>>() }]);
    }
    body
}

fn ollama_body(cfg: &ProviderConfig, i: &ChatInput<'_>) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": i.system })];
    for m in i.msgs {
        match m {
            Msg::User { text } => messages.push(json!({ "role": "user", "content": text })),
            Msg::Assistant { text, tool_calls } => {
                let mut v = json!({ "role": "assistant", "content": text });
                if !tool_calls.is_empty() {
                    v["tool_calls"] = json!(tool_calls
                        .iter()
                        .map(|c| json!({ "function": { "name": wire_name(&c.name), "arguments": c.args } }))
                        .collect::<Vec<_>>());
                }
                messages.push(v);
            }
            Msg::ToolResult { name, text, .. } => messages
                .push(json!({ "role": "tool", "tool_name": wire_name(name), "content": text })),
        }
    }
    let mut body = json!({ "model": cfg.model, "messages": messages, "stream": true, "options": { "num_predict": i.max_output } });
    if !i.tools.is_empty() {
        body["tools"] = json!(i
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "function": { "name": wire_name(&t.name), "description": t.description, "parameters": t.schema } }))
            .collect::<Vec<_>>());
    }
    body
}

// ------------------------------------------------------------------------------------------- the JSON protocol

/// The instruction that replaces native tool calling for a model without it.
#[must_use]
pub fn json_protocol_prompt(tools: &[WireTool]) -> String {
    let mut s = String::from(
        "\n\nThis model cannot call tools natively. To call a tool, answer with ONLY one JSON object and nothing else: {\"tool\": \"<name>\", \"arguments\": {...}}. After the result arrives you may call another tool or answer in plain text. Tools:\n",
    );
    for t in tools {
        s.push_str(&format!(
            "- {}: {} Arguments schema: {}\n",
            t.name, t.description, t.schema
        ));
    }
    s
}

/// A tool call written as JSON text by a model that has no native tool calling.
#[must_use]
pub fn parse_json_call(text: &str, tools: &[WireTool]) -> Option<ToolCall> {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    let v: Value = serde_json::from_str(t).ok()?;
    let name = v.get("tool")?.as_str()?;
    tools.iter().find(|x| x.name == name)?;
    Some(ToolCall {
        id: format!(
            "call_{}",
            blake3::hash(t.as_bytes())
                .to_hex()
                .chars()
                .take(12)
                .collect::<String>()
        ),
        name: name.to_owned(),
        args: v.get("arguments").cloned().unwrap_or_else(|| json!({})),
    })
}

// ------------------------------------------------------------------------------------------------- transport

fn auth_headers(cfg: &ProviderConfig, key: Option<&str>) -> Result<HeaderMap, ProviderError> {
    let mut h = HeaderMap::new();
    h.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(k) = key {
        let (name, value) = match cfg.kind {
            ProviderKind::Anthropic => (HeaderName::from_static("x-api-key"), k.to_owned()),
            ProviderKind::Google => (HeaderName::from_static("x-goog-api-key"), k.to_owned()),
            _ => (reqwest::header::AUTHORIZATION, format!("Bearer {k}")),
        };
        let mut hv = HeaderValue::from_str(&value).map_err(|_| ProviderError::Auth)?;
        hv.set_sensitive(true);
        h.insert(name, hv);
    }
    if cfg.kind == ProviderKind::Anthropic {
        let v = cfg
            .api_version
            .as_deref()
            .unwrap_or(ogun_llm::config::DEFAULT_ANTHROPIC_VERSION);
        h.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_str(v)
                .map_err(|_| ProviderError::Rejected("bad api_version".into()))?,
        );
    }
    Ok(h)
}

fn endpoint(cfg: &ProviderConfig) -> Result<String, ProviderError> {
    let base = cfg
        .base()
        .map_err(|_| ProviderError::Rejected("no base URL".into()))?;
    let base = base.as_str().trim_end_matches('/').to_owned();
    Ok(match cfg.kind {
        ProviderKind::Anthropic => format!("{base}/v1/messages"),
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => format!("{base}/chat/completions"),
        ProviderKind::Ollama => format!("{base}/api/chat"),
        ProviderKind::Google => {
            let m = if cfg.model.starts_with("models/") {
                cfg.model.clone()
            } else {
                format!("models/{}", cfg.model)
            };
            format!("{base}/{m}:streamGenerateContent?alt=sse")
        }
    })
}

async fn classify(
    status: reqwest::StatusCode,
    retry_after: Option<u64>,
    resp: reqwest::Response,
) -> ProviderError {
    let body = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
    let v: Value =
        serde_json::from_slice(&body[..body.len().min(64 * 1024)]).unwrap_or(Value::Null);
    let kind = ident(&v["error"]["type"])
        .or_else(|| ident(&v["error"]["code"]))
        .or_else(|| ident(&v["error"]["status"]));
    let text = String::from_utf8_lossy(&body[..body.len().min(4096)]).to_lowercase();
    match status.as_u16() {
        401 | 403 => ProviderError::Auth,
        429 => ProviderError::RateLimited(retry_after),
        408 | 500..=599 => ProviderError::Unavailable(format!(
            "HTTP {}{}",
            status.as_u16(),
            kind.map_or(String::new(), |k| format!(" {k}"))
        )),
        400 if text.contains("tool")
            && (text.contains("support") || text.contains("not enabled")) =>
        {
            ProviderError::NoTools
        }
        s => ProviderError::Rejected(format!(
            "HTTP {s}{}",
            kind.map_or(String::new(), |k| format!(" {k}"))
        )),
    }
}

/// Reads server-sent events (or NDJSON lines when `ndjson`) and hands each to `f` as `(event name, data)`.
async fn read_stream(
    resp: reqwest::Response,
    ndjson: bool,
    cancel: &mut watch::Receiver<bool>,
    mut f: impl FnMut(Option<&str>, &str) -> Result<bool, ProviderError>,
) -> Result<(), ProviderError> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = tokio::select! {
            c = stream.next() => c,
            _ = cancel.changed() => {
                if *cancel.borrow() { return Err(ProviderError::Cancelled); }
                continue;
            }
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|_| ProviderError::Unavailable("the stream broke".into()))?;
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_EVENT {
            return Err(ProviderError::Unavailable(
                "an event was larger than the limit".into(),
            ));
        }
        loop {
            let (end, skip) = if ndjson {
                match buf.iter().position(|b| *b == b'\n') {
                    Some(p) => (p, 1),
                    None => break,
                }
            } else {
                match find_event_end(&buf) {
                    Some(x) => x,
                    None => break,
                }
            };
            let raw: Vec<u8> = buf.drain(..end + skip).take(end).collect();
            let text = String::from_utf8_lossy(&raw);
            if ndjson {
                if !text.trim().is_empty() && f(None, text.trim())? {
                    return Ok(());
                }
                continue;
            }
            let mut event: Option<String> = None;
            let mut data = String::new();
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = Some(v.trim().to_owned());
                } else if let Some(v) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(v.strip_prefix(' ').unwrap_or(v));
                }
            }
            if !data.is_empty() && f(event.as_deref(), &data)? {
                return Ok(());
            }
        }
    }
    if !ndjson && !buf.iter().all(u8::is_ascii_whitespace) {
        // A final event without its blank line.
        let text = String::from_utf8_lossy(&buf).into_owned();
        let data: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|v| v.strip_prefix(' ').unwrap_or(v))
            .collect::<Vec<_>>()
            .join("\n");
        if !data.is_empty() {
            f(None, &data)?;
        }
    }
    Ok(())
}

/// The end of the first event in `buf`: the blank line that closes it (`\n\n` or `\r\n\r\n`).
fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if buf[i] == b'\r' && i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

#[derive(Default)]
struct Acc {
    text: String,
    calls: Vec<(String, String, String)>, // (id, wire name, argument text)
    input: Option<u64>,
    output: Option<u64>,
    finish: Option<Finish>,
    refused: Option<String>,
}

impl Acc {
    fn push_text(&mut self, t: &str, on_text: &mut (dyn FnMut(&str) + Send)) {
        if !t.is_empty() && self.text.len() + t.len() <= MAX_TEXT {
            self.text.push_str(t);
            on_text(t);
        }
    }
}

fn u64_of(v: &Value) -> Option<u64> {
    v.as_u64()
}

fn finish_calls(acc: &Acc, json_text_params: bool) -> Vec<ToolCall> {
    acc.calls
        .iter()
        .enumerate()
        .filter(|(_, (_, name, _))| !name.is_empty())
        .map(|(n, (id, name, args))| {
            let mut v: Value = if args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(args).unwrap_or_else(|_| json!({ "_unparsed": true }))
            };
            if json_text_params && let Some(o) = v.as_object_mut() {
                // Gemini: open objects travelled as JSON text.
                for x in o.values_mut() {
                    if let Some(t) = x.as_str().map(str::to_owned)
                        && (t.starts_with('{') || t.starts_with('['))
                        && let Ok(parsed) = serde_json::from_str::<Value>(&t)
                    {
                        *x = parsed;
                    }
                }
            }
            ToolCall {
                id: if id.is_empty() {
                    format!("call_{n}")
                } else {
                    id.clone()
                },
                name: from_wire(name),
                args: v,
            }
        })
        .collect()
}

/// Streams one chat completion. `on_text` sees every piece of text as it arrives.
pub async fn stream_chat(
    cfg: &ProviderConfig,
    key: Option<&str>,
    input: &ChatInput<'_>,
    cancel: &mut watch::Receiver<bool>,
    on_text: &mut (dyn FnMut(&str) + Send),
) -> Result<Completion, ProviderError> {
    let http = client(cfg)?;
    let url = endpoint(cfg)?;
    let headers = auth_headers(cfg, key)?;
    let mut with_usage = true;
    let mut attempt = 0;
    let resp = loop {
        let body = match cfg.kind {
            ProviderKind::Anthropic => anthropic_body(cfg, input),
            ProviderKind::Openai | ProviderKind::OpenaiCompatible => {
                openai_body(cfg, input, with_usage)
            }
            ProviderKind::Google => gemini_body(input),
            ProviderKind::Ollama => ollama_body(cfg, input),
        };
        let send = http.post(&url).headers(headers.clone()).json(&body).send();
        let resp = tokio::select! {
            r = send => r.map_err(|e| ProviderError::Unavailable(
                if e.is_timeout() { "timed out".into() } else if e.is_connect() { "cannot connect".into() } else { "the request failed".into() })),
            _ = wait_cancel(cancel) => return Err(ProviderError::Cancelled),
        }?;
        if resp.status().is_success() {
            break resp;
        }
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|s| s.saturating_mul(1000));
        let err = classify(status, retry_after, resp).await;
        attempt += 1;
        match &err {
            ProviderError::Rejected(_)
                if with_usage && matches!(cfg.kind, ProviderKind::OpenaiCompatible) =>
            {
                // A server that does not know `stream_options`.
                with_usage = false;
            }
            ProviderError::RateLimited(_) | ProviderError::Unavailable(_) if attempt < ATTEMPTS => {
                let wait = match &err {
                    ProviderError::RateLimited(Some(ms)) => (*ms).min(20_000),
                    _ => 500u64 << attempt,
                };
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(wait)) => {}
                    _ = wait_cancel(cancel) => return Err(ProviderError::Cancelled),
                }
            }
            _ => return Err(err),
        }
    };
    let mut acc = Acc::default();
    let mut json_text_params = false;
    let kind = cfg.kind;
    let ndjson = kind == ProviderKind::Ollama;
    // Anthropic content block index -> index in `acc.calls` (only tool_use blocks).
    let mut block_to_call: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let r = read_stream(resp, ndjson, cancel, |event, data| {
        if data == "[DONE]" {
            return Ok(true);
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        match kind {
            ProviderKind::Anthropic => {
                let t = event
                    .map(str::to_owned)
                    .or_else(|| v["type"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                match t.as_str() {
                    "message_start" => {
                        let u = &v["message"]["usage"];
                        let total = u64_of(&u["input_tokens"]).unwrap_or(0)
                            + u64_of(&u["cache_creation_input_tokens"]).unwrap_or(0)
                            + u64_of(&u["cache_read_input_tokens"]).unwrap_or(0);
                        acc.input = Some(total);
                        acc.output = u64_of(&u["output_tokens"]);
                    }
                    "content_block_start" => {
                        let idx = v["index"].as_u64().unwrap_or(0);
                        let b = &v["content_block"];
                        if b["type"] == "tool_use" {
                            acc.calls.push((
                                b["id"].as_str().unwrap_or("").to_owned(),
                                b["name"].as_str().unwrap_or("").to_owned(),
                                String::new(),
                            ));
                            block_to_call.insert(idx, acc.calls.len() - 1);
                        } else if let Some(text) = b["text"].as_str().filter(|t| !t.is_empty()) {
                            acc.push_text(text, on_text);
                        }
                    }
                    "content_block_delta" => {
                        let idx = v["index"].as_u64().unwrap_or(0);
                        let d = &v["delta"];
                        match d["type"].as_str() {
                            Some("text_delta") => {
                                acc.push_text(d["text"].as_str().unwrap_or(""), on_text)
                            }
                            Some("input_json_delta") => {
                                if let Some(&c) = block_to_call.get(&idx) {
                                    acc.calls[c]
                                        .2
                                        .push_str(d["partial_json"].as_str().unwrap_or(""));
                                }
                            }
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        if let Some(r) = v["delta"]["stop_reason"].as_str() {
                            acc.finish = Some(match r {
                                "tool_use" => Finish::ToolUse,
                                "max_tokens" => Finish::Length,
                                _ => Finish::Stop,
                            });
                            if r == "refusal" {
                                acc.refused = Some(
                                    ident(&v["delta"]["stop_details"]["category"])
                                        .unwrap_or_else(|| "refusal".into()),
                                );
                            }
                        }
                        if let Some(o) = u64_of(&v["usage"]["output_tokens"]) {
                            acc.output = Some(o);
                        }
                        if let Some(i) = u64_of(&v["usage"]["input_tokens"]) {
                            acc.input = Some(i);
                        }
                    }
                    "message_stop" => return Ok(true),
                    "error" => {
                        let ty = ident(&v["error"]["type"]).unwrap_or_else(|| "error".into());
                        return Err(if ty == "overloaded_error" || ty == "api_error" {
                            ProviderError::Unavailable(ty)
                        } else if ty == "rate_limit_error" {
                            ProviderError::RateLimited(None)
                        } else {
                            ProviderError::Rejected(ty)
                        });
                    }
                    _ => {}
                }
            }
            ProviderKind::Openai | ProviderKind::OpenaiCompatible => {
                if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                    acc.input = u64_of(&u["prompt_tokens"]).or(acc.input);
                    acc.output = u64_of(&u["completion_tokens"]).or(acc.output);
                }
                let choice = &v["choices"][0];
                if choice.is_null() {
                    return Ok(false);
                }
                let d = &choice["delta"];
                if let Some(t) = d["content"].as_str() {
                    acc.push_text(t, on_text);
                }
                if let Some(r) = d["refusal"].as_str().filter(|r| !r.is_empty()) {
                    let _ = r;
                    acc.refused = Some("refusal".into());
                }
                for tc in d["tool_calls"].as_array().into_iter().flatten() {
                    let i = tc["index"].as_u64().unwrap_or(0) as usize;
                    if i > 31 {
                        continue;
                    }
                    while acc.calls.len() <= i {
                        acc.calls
                            .push((String::new(), String::new(), String::new()));
                    }
                    if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
                        acc.calls[i].0 = id.to_owned();
                    }
                    if let Some(n) = tc["function"]["name"].as_str().filter(|s| !s.is_empty()) {
                        acc.calls[i].1.push_str(n);
                    }
                    if let Some(a) = tc["function"]["arguments"].as_str() {
                        acc.calls[i].2.push_str(a);
                    }
                }
                if let Some(f) = choice["finish_reason"].as_str() {
                    acc.finish = Some(match f {
                        "tool_calls" | "function_call" => Finish::ToolUse,
                        "length" => Finish::Length,
                        "content_filter" => {
                            acc.refused = Some("content_filter".into());
                            Finish::Stop
                        }
                        _ => Finish::Stop,
                    });
                }
            }
            ProviderKind::Google => {
                if let Some(u) = v.get("usageMetadata") {
                    acc.input = u64_of(&u["promptTokenCount"]).or(acc.input);
                    acc.output = u64_of(&u["candidatesTokenCount"])
                        .map(|c| c + u64_of(&u["thoughtsTokenCount"]).unwrap_or(0))
                        .or(acc.output);
                }
                if let Some(r) = v["promptFeedback"]["blockReason"].as_str() {
                    acc.refused = Some(ident(&json!(r)).unwrap_or_else(|| "blocked".into()));
                }
                let cand = &v["candidates"][0];
                for part in cand["content"]["parts"].as_array().into_iter().flatten() {
                    if let Some(t) = part["text"].as_str() {
                        if part["thought"] != json!(true) {
                            acc.push_text(t, on_text);
                        }
                    } else if part["functionCall"].is_object() {
                        let fc = &part["functionCall"];
                        acc.calls.push((
                            String::new(),
                            fc["name"].as_str().unwrap_or("").to_owned(),
                            fc["args"].to_string(),
                        ));
                        json_text_params = true;
                    }
                }
                if let Some(f) = cand["finishReason"].as_str() {
                    acc.finish = Some(match f {
                        "MAX_TOKENS" => Finish::Length,
                        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
                            acc.refused = Some(f.to_lowercase());
                            Finish::Stop
                        }
                        _ => Finish::Stop,
                    });
                }
            }
            ProviderKind::Ollama => {
                if let Some(t) = v["message"]["content"].as_str() {
                    acc.push_text(t, on_text);
                }
                for tc in v["message"]["tool_calls"].as_array().into_iter().flatten() {
                    acc.calls.push((
                        String::new(),
                        tc["function"]["name"].as_str().unwrap_or("").to_owned(),
                        tc["function"]["arguments"].to_string(),
                    ));
                }
                if v["done"] == json!(true) {
                    acc.input = u64_of(&v["prompt_eval_count"]).or(acc.input);
                    acc.output = u64_of(&v["eval_count"]).or(acc.output);
                    acc.finish = Some(if v["done_reason"] == "length" {
                        Finish::Length
                    } else {
                        Finish::Stop
                    });
                    return Ok(true);
                }
            }
        }
        Ok(false)
    })
    .await;
    r?;
    if let Some(why) = acc.refused.take() {
        return Err(ProviderError::Refused(why));
    }
    // Gemini reports only `functionCall` parts; Anthropic and OpenAI name the finish reason.
    let calls = finish_calls(&acc, json_text_params);
    let reported = acc.input.is_some() && acc.output.is_some();
    let finish = if !calls.is_empty() {
        Finish::ToolUse
    } else {
        acc.finish.unwrap_or(Finish::Stop)
    };
    Ok(Completion {
        input_tokens: acc.input.unwrap_or_else(|| estimate(msgs_bytes(input))),
        output_tokens: acc.output.unwrap_or_else(|| {
            estimate(
                acc.text.len()
                    + calls
                        .iter()
                        .map(|c| c.args.to_string().len())
                        .sum::<usize>(),
            )
        }),
        reported,
        text: acc.text,
        calls,
        finish,
    })
}

async fn wait_cancel(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow() {
            return;
        }
        if cancel.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_end_at_a_blank_line_either_style() {
        assert_eq!(find_event_end(b"data: a\n\ndata: b\n\n"), Some((7, 2)));
        assert_eq!(find_event_end(b"data: a\r\n\r\nrest"), Some((7, 4)));
        assert_eq!(find_event_end(b"data: partial"), None);
    }

    #[test]
    fn gemini_schemas_have_no_open_objects() {
        let s = gemini_schema(
            &json!({"type":"object","properties":{"workflow":{"type":"object","description":"doc"}},"required":["workflow"]}),
        );
        assert_eq!(s["properties"]["workflow"]["type"], "string");
        assert_eq!(s["type"], "object");
    }

    #[test]
    fn a_json_protocol_call_is_found_in_fences_or_bare() {
        let tools = vec![WireTool {
            name: "status.get".into(),
            description: String::new(),
            schema: json!({}),
        }];
        let c = parse_json_call(
            "```json\n{\"tool\":\"status.get\",\"arguments\":{}}\n```",
            &tools,
        )
        .unwrap();
        assert_eq!(c.name, "status.get");
        assert!(parse_json_call("{\"tool\":\"shell\",\"arguments\":{}}", &tools).is_none());
        assert!(parse_json_call("Here is the status.", &tools).is_none());
    }

    #[test]
    fn provider_error_sentences_carry_no_provider_text() {
        let s = ProviderError::Rejected("HTTP 400 invalid_request_error".into())
            .sentence("anthropic-sonnet");
        assert!(s.contains("anthropic-sonnet") && s.contains("invalid_request_error"));
    }
}
