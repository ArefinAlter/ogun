//! Provider-neutral request and response types.

use serde::Serialize;
use serde_json::Value;

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The pipeline (documents, repair requests).
    User,
    /// The model (an earlier answer, during a repair).
    Assistant,
}

/// One conversation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Author.
    pub role: Role,
    /// Text.
    pub content: String,
}

impl Message {
    /// A user message.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    /// An assistant message.
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// One chat call.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    /// System prompt: frame, task and approved configuration. Never document text.
    pub system: String,
    /// The conversation; the first message carries the documents.
    pub messages: Vec<Message>,
    /// The JSON Schema the answer must satisfy, when structured output is wanted.
    pub schema: Option<Value>,
    /// Output limit for this call; the provider's `max_output_tokens` caps it.
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature in thousandths (IR documents hold no floats). Ignored by Anthropic, whose current
    /// API version accepts no sampling parameters.
    pub temperature_milli: Option<u32>,
}

impl ChatRequest {
    /// Bytes of everything sent, for the worst-case input bound.
    #[must_use]
    pub fn prompt_bytes(&self) -> u64 {
        let schema = self
            .schema
            .as_ref()
            .map_or(0, |s| serde_json::to_vec(s).map_or(0, |v| v.len()));
        (self.system.len() + schema + self.messages.iter().map(|m| m.content.len()).sum::<usize>())
            as u64
    }
}

/// Why generation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// The model finished.
    Stop,
    /// The output limit was reached; the answer may be cut off.
    Length,
}

/// One call's usage, as emitted on a `usage` line and recorded in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Usage {
    /// Provider name from the ladder.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens, including reasoning tokens.
    pub output_tokens: u64,
    /// Cost in micro-US-dollars.
    pub cost_microusd: u64,
}

/// The answer to a chat call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    /// The text of the answer (reasoning blocks are not included).
    pub text: String,
    /// Why it stopped.
    pub finish: Finish,
    /// What it cost.
    pub usage: Usage,
}

/// One embeddings call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedRequest {
    /// Texts to embed, in order.
    pub inputs: Vec<String>,
    /// Requested vector size, where the provider supports choosing it.
    pub dimensions: Option<u32>,
}

impl EmbedRequest {
    /// Bytes of the inputs.
    #[must_use]
    pub fn input_bytes(&self) -> u64 {
        self.inputs.iter().map(|s| s.len() as u64).sum()
    }
}

/// The answer to an embeddings call.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedResponse {
    /// One vector per input, in input order.
    pub vectors: Vec<Vec<f32>>,
    /// What it cost.
    pub usage: Usage,
}
