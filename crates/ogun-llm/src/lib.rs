//! Ogun's model layer (O-03 section 8; constraint 6: models are optional).
//!
//! - [`config`]: the provider ladder, `<state_dir>/llm.json`, and what a runner injects into a model
//!   operator's start line.
//! - [`budget`]: integer micro-dollar prices, worst-case bounds and the per-attempt call budget.
//! - [`ledger`]: the append-only, hash-chained spend ledger `<state_dir>/llm-ledger.jsonl` with summaries per
//!   run, workflow and UTC day.
//! - [`prompt`]: prompt construction that keeps untrusted documents out of the system prompt.
//! - [`schema`]: structured output: compile, parse, validate, adapt to a provider's native mode.
//! - [`secrets`]: zeroizing API keys that cannot be printed or serialised.
//! - `provider` and `ladder` (feature `http`): the HTTPS clients for Anthropic, OpenAI, OpenAI-compatible
//!   servers, Ollama and Google Gemini, and the runner that falls back down the ladder on transient failures.
//!
//! With no `llm.json` (or an empty ladder) nothing here makes a network request. The daemon needs only the
//! default features; the `ogun-op-llm` child operator enables `http`.

#![forbid(unsafe_code)]

pub mod budget;
pub mod config;
pub mod error;
pub mod ledger;
pub mod prompt;
pub mod request;
pub mod schema;
pub mod secrets;

#[cfg(feature = "http")]
pub mod ladder;
#[cfg(feature = "http")]
pub mod provider;

pub use budget::{CallBudget, check_budget, cost_usd_micro, worst_case_usd_micro};
pub use config::{Capability, LadderConfig, ProviderConfig, ProviderKind};
pub use error::LlmError;
pub use ledger::{Ledger, LedgerEntry, Summary, UsageRecord};
pub use request::{
    ChatRequest, ChatResponse, EmbedRequest, EmbedResponse, Finish, Message, Role, Usage,
};
pub use schema::Schema;
pub use secrets::{ApiKey, KeyMap, KeyResolver};

#[cfg(feature = "http")]
pub use ladder::{JsonAnswer, Ladder};
#[cfg(feature = "http")]
pub use provider::Provider;
