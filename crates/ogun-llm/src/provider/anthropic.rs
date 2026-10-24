//! Anthropic Messages API (`POST /v1/messages`).
//!
//! Structured output uses `output_config.format` with a `json_schema` (the adapted schema; validation is
//! local). Sampling parameters are not sent: the API version this build speaks rejects them. Reasoning
//! (`thinking`) blocks are skipped; their tokens are billed as output tokens and counted by the provider.

use serde_json::{Value, json};

use super::{Auth, CHAT_BODY_LIMIT, Provider, count};
use crate::config::{DEFAULT_ANTHROPIC_VERSION, ProviderKind};
use crate::error::LlmError;
use crate::request::{ChatRequest, ChatResponse, Finish, Role};
use crate::schema;

pub(super) async fn chat(p: &Provider, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": m.content,
            })
        })
        .collect();
    let mut body = json!({
        "model": p.cfg.model,
        "max_tokens": p.max_output(req.max_output_tokens),
        "system": req.system,
        "messages": messages,
    });
    if let Some(s) = &req.schema {
        body["output_config"] = json!({
            "format": { "type": "json_schema", "schema": schema::for_provider(ProviderKind::Anthropic, s) }
        });
    }
    let version = p
        .cfg
        .api_version
        .as_deref()
        .unwrap_or(DEFAULT_ANTHROPIC_VERSION);
    let v = p
        .post(
            "/v1/messages",
            &body,
            Auth::Header("x-api-key"),
            &[("anthropic-version", version)],
            CHAT_BODY_LIMIT,
        )
        .await?;
    let stop = v["stop_reason"].as_str().unwrap_or("");
    if stop == "refusal" {
        let category = v["stop_details"]["category"].as_str().unwrap_or("refusal");
        return Err(p.refused(category));
    }
    let blocks = v["content"]
        .as_array()
        .ok_or_else(|| p.malformed("no content"))?;
    let mut text = String::new();
    for block in blocks {
        if block["type"] == "text"
            && let Some(t) = block["text"].as_str()
        {
            text.push_str(t);
        }
    }
    let u = &v["usage"];
    let input = count(&u["input_tokens"]).map(|n| {
        n.saturating_add(count(&u["cache_creation_input_tokens"]).unwrap_or(0))
            .saturating_add(count(&u["cache_read_input_tokens"]).unwrap_or(0))
    });
    let output = count(&u["output_tokens"]);
    let usage = p.usage(input, output, req.prompt_bytes(), text.len() as u64);
    Ok(ChatResponse {
        finish: if stop == "max_tokens" {
            Finish::Length
        } else {
            Finish::Stop
        },
        text,
        usage,
    })
}
