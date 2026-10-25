//! Ollama (`POST {base}/api/chat`, `POST {base}/api/embed`). Structured output passes the schema as `format`.

use serde_json::json;

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
    let mut options = json!({ "num_predict": p.max_output(req.max_output_tokens) });
    if let Some(t) = req.temperature_milli {
        options["temperature"] = temperature(t);
    }
    let mut body = json!({
        "model": p.cfg.model,
        "messages": messages,
        "stream": false,
        "options": options,
    });
    if let Some(s) = &req.schema {
        body["format"] = schema::for_provider(ProviderKind::Ollama, s);
    }
    let v = p
        .post("/api/chat", &body, Auth::Bearer, &[], CHAT_BODY_LIMIT)
        .await?;
    let text = v["message"]["content"]
        .as_str()
        .ok_or_else(|| p.malformed("no message"))?
        .to_owned();
    let usage = p.usage(
        count(&v["prompt_eval_count"]),
        count(&v["eval_count"]),
        req.prompt_bytes(),
        text.len() as u64,
    );
    Ok(ChatResponse {
        finish: if v["done_reason"] == "length" {
            Finish::Length
        } else {
            Finish::Stop
        },
        text,
        usage,
    })
}

pub(super) async fn embed(p: &Provider, req: &EmbedRequest) -> Result<EmbedResponse, LlmError> {
    let mut body = json!({ "model": p.cfg.model, "input": req.inputs });
    if let Some(d) = req.dimensions {
        body["dimensions"] = json!(d);
    }
    let v = p
        .post("/api/embed", &body, Auth::Bearer, &[], EMBED_BODY_LIMIT)
        .await?;
    let vectors = v["embeddings"]
        .as_array()
        .ok_or_else(|| p.malformed("no embeddings"))?
        .iter()
        .map(vector)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| p.malformed("embedding"))?;
    let usage = p.usage(
        count(&v["prompt_eval_count"]),
        Some(0),
        req.input_bytes(),
        0,
    );
    Ok(EmbedResponse { vectors, usage })
}
