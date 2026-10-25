//! Google Gemini (`POST {base}/models/{model}:generateContent`, `...:batchEmbedContents`). The key travels in
//! the `x-goog-api-key` header, never in the query string.

use serde_json::{Value, json};

use super::{Auth, CHAT_BODY_LIMIT, EMBED_BODY_LIMIT, Provider, count, temperature, vector};
use crate::config::ProviderKind;
use crate::error::LlmError;
use crate::request::{ChatRequest, ChatResponse, EmbedRequest, EmbedResponse, Finish, Role};
use crate::schema;

const BLOCKED: &[&str] = &[
    "SAFETY",
    "RECITATION",
    "BLOCKLIST",
    "PROHIBITED_CONTENT",
    "SPII",
    "IMAGE_SAFETY",
];

fn model_path(p: &Provider) -> String {
    if p.cfg.model.starts_with("models/") {
        p.cfg.model.clone()
    } else {
        format!("models/{}", p.cfg.model)
    }
}

pub(super) async fn chat(p: &Provider, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
    let contents: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "model" },
                "parts": [{ "text": m.content }],
            })
        })
        .collect();
    let mut generation = json!({ "maxOutputTokens": p.max_output(req.max_output_tokens) });
    if let Some(t) = req.temperature_milli {
        generation["temperature"] = temperature(t);
    }
    if let Some(s) = &req.schema {
        generation["responseMimeType"] = json!("application/json");
        generation["responseJsonSchema"] = schema::for_provider(ProviderKind::Google, s);
    }
    let body = json!({
        "systemInstruction": { "parts": [{ "text": req.system }] },
        "contents": contents,
        "generationConfig": generation,
    });
    let path = format!("/{}:generateContent", model_path(p));
    let v = p
        .post(
            &path,
            &body,
            Auth::Header("x-goog-api-key"),
            &[],
            CHAT_BODY_LIMIT,
        )
        .await?;
    if let Some(reason) = v["promptFeedback"]["blockReason"].as_str() {
        return Err(p.refused(reason));
    }
    let candidate = &v["candidates"][0];
    if candidate.is_null() {
        return Err(p.malformed("no candidates"));
    }
    let finish = candidate["finishReason"].as_str().unwrap_or("STOP");
    if BLOCKED.contains(&finish) {
        return Err(p.refused(finish));
    }
    let mut text = String::new();
    if let Some(parts) = candidate["content"]["parts"].as_array() {
        for part in parts {
            if part["thought"] == true {
                continue;
            }
            if let Some(t) = part["text"].as_str() {
                text.push_str(t);
            }
        }
    }
    let meta = &v["usageMetadata"];
    let output = count(&meta["candidatesTokenCount"])
        .map(|n| n.saturating_add(count(&meta["thoughtsTokenCount"]).unwrap_or(0)));
    let usage = p.usage(
        count(&meta["promptTokenCount"]),
        output,
        req.prompt_bytes(),
        text.len() as u64,
    );
    Ok(ChatResponse {
        finish: if finish == "MAX_TOKENS" {
            Finish::Length
        } else {
            Finish::Stop
        },
        text,
        usage,
    })
}

pub(super) async fn embed(p: &Provider, req: &EmbedRequest) -> Result<EmbedResponse, LlmError> {
    let model = model_path(p);
    let requests: Vec<Value> = req
        .inputs
        .iter()
        .map(|t| {
            let mut r = json!({ "model": model, "content": { "parts": [{ "text": t }] } });
            if let Some(d) = req.dimensions {
                r["outputDimensionality"] = json!(d);
            }
            r
        })
        .collect();
    let path = format!("/{model}:batchEmbedContents");
    let v = p
        .post(
            &path,
            &json!({ "requests": requests }),
            Auth::Header("x-goog-api-key"),
            &[],
            EMBED_BODY_LIMIT,
        )
        .await?;
    let vectors = v["embeddings"]
        .as_array()
        .ok_or_else(|| p.malformed("no embeddings"))?
        .iter()
        .map(|e| vector(&e["values"]))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| p.malformed("embedding"))?;
    // batchEmbedContents reports no token counts; the input is estimated from bytes.
    let usage = p.usage(None, Some(0), req.input_bytes(), 0);
    Ok(EmbedResponse { vectors, usage })
}
