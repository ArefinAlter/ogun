//! OpenAI Chat Completions (`POST {base}/chat/completions`) and Embeddings (`POST {base}/embeddings`), also
//! used for `openai_compatible` endpoints (vLLM, llama.cpp server, LM Studio, OpenRouter).

use serde_json::{Value, json};

use super::{Auth, CHAT_BODY_LIMIT, EMBED_BODY_LIMIT, Provider, count, temperature, vector};
use crate::config::ProviderKind;
use crate::error::LlmError;
use crate::request::{ChatRequest, ChatResponse, EmbedRequest, EmbedResponse, Finish, Role};
use crate::schema;

pub(super) async fn chat(p: &Provider, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
    let mut messages = vec![json!({"role": "system", "content": req.system})];
    messages.extend(req.messages.iter().map(|m| {
        json!({
            "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
            "content": m.content,
        })
    }));
    let limit = p.max_output(req.max_output_tokens);
    let mut body = json!({ "model": p.cfg.model, "messages": messages, "stream": false });
    // OpenAI's current models take `max_completion_tokens`; compatible servers know `max_tokens`.
    if p.cfg.kind == ProviderKind::Openai {
        body["max_completion_tokens"] = json!(limit);
    } else {
        body["max_tokens"] = json!(limit);
    }
    if let Some(t) = req.temperature_milli {
        body["temperature"] = temperature(t);
    }
    if let Some(s) = &req.schema {
        let adapted = schema::for_provider(p.cfg.kind, s);
        let strict = schema::strict_compatible(&adapted);
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": { "name": "ogun_output", "schema": adapted, "strict": strict }
        });
    }
    let v = p
        .post(
            "/chat/completions",
            &body,
            Auth::Bearer,
            &[],
            CHAT_BODY_LIMIT,
        )
        .await?;
    let choice = &v["choices"][0];
    if choice.is_null() {
        return Err(p.malformed("no choices"));
    }
    if let Some(r) = choice["message"]["refusal"].as_str()
        && !r.is_empty()
    {
        return Err(p.refused("refusal"));
    }
    let finish = choice["finish_reason"].as_str().unwrap_or("stop");
    if finish == "content_filter" {
        return Err(p.refused("content_filter"));
    }
    let text = match &choice["message"]["content"] {
        Value::String(s) => s.clone(),
        // Some servers return content parts.
        Value::Array(parts) => parts
            .iter()
            .filter_map(|x| x["text"].as_str())
            .collect::<String>(),
        _ => String::new(),
    };
    let usage = p.usage(
        count(&v["usage"]["prompt_tokens"]),
        count(&v["usage"]["completion_tokens"]),
        req.prompt_bytes(),
        text.len() as u64,
    );
    Ok(ChatResponse {
        finish: if finish == "length" {
            Finish::Length
        } else {
            Finish::Stop
        },
        text,
        usage,
    })
}

pub(super) async fn embed(p: &Provider, req: &EmbedRequest) -> Result<EmbedResponse, LlmError> {
    let mut body = json!({ "model": p.cfg.model, "input": req.inputs, "encoding_format": "float" });
    if let Some(d) = req.dimensions {
        body["dimensions"] = json!(d);
    }
    let v = p
        .post("/embeddings", &body, Auth::Bearer, &[], EMBED_BODY_LIMIT)
        .await?;
    let data = v["data"].as_array().ok_or_else(|| p.malformed("no data"))?;
    let mut indexed: Vec<(u64, Vec<f32>)> = Vec::with_capacity(data.len());
    for (i, item) in data.iter().enumerate() {
        let index = item["index"].as_u64().unwrap_or(i as u64);
        let vec = vector(&item["embedding"]).ok_or_else(|| p.malformed("embedding"))?;
        indexed.push((index, vec));
    }
    indexed.sort_by_key(|(i, _)| *i);
    let usage = p.usage(
        count(&v["usage"]["prompt_tokens"]),
        Some(0),
        req.input_bytes(),
        0,
    );
    Ok(EmbedResponse {
        vectors: indexed.into_iter().map(|(_, v)| v).collect(),
        usage,
    })
}
