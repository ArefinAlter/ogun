//! The shapes of threads, items, settings and the model transcript (O-17 section 2).

use ogun_types::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Access levels, least to most.
pub const ACCESS: [&str; 3] = ["read", "author", "operate"];

/// The rank of an access level; `None` for a word that is not one.
#[must_use]
pub fn rank(access: &str) -> Option<usize> {
    ACCESS.iter().position(|a| *a == access)
}

/// Something a person pointed at with `@`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    /// `workflow`, `run`, `connector` or `approval`.
    pub kind: String,
    /// Its id.
    pub id: String,
    /// What the person saw.
    pub label: String,
}

/// One item of a thread, as the interface draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// `itm_` and a ULID.
    pub id: String,
    /// When.
    pub at: Timestamp,
    /// What it is.
    #[serde(flatten)]
    pub body: ItemBody,
}

/// The kinds of item (O-17 section 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    /// A person's message.
    User {
        /// The text.
        text: String,
        /// What they pointed at.
        mentions: Vec<Mention>,
    },
    /// The agent's words: untrusted text.
    Assistant {
        /// The text so far.
        text: String,
        /// More is coming.
        streaming: bool,
    },
    /// One tool call.
    Tool {
        /// The tool.
        tool: String,
        /// One line.
        summary: String,
        /// The arguments, bounded.
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
        /// The daemon's answer, bounded.
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
        /// `running`, `ok`, `error` or `denied`.
        state: String,
        /// How long it took.
        #[serde(skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
    },
    /// A card the daemon built from a tool result.
    Card {
        /// The card.
        card: Value,
    },
    /// The daemon's own line.
    Notice {
        /// `info` or `warn`.
        tone: String,
        /// The text.
        text: String,
    },
}

/// A thread, summary members first (O-17 section 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    /// `thr_` and a ULID.
    pub thread_id: String,
    /// The first request, cut at a word.
    pub title: String,
    /// `idle`, `running` or `waiting`.
    pub state: String,
    /// `read`, `author` or `operate`.
    pub access: String,
    /// The provider name in the ladder.
    pub model: String,
    /// Created.
    pub created_at: Timestamp,
    /// Last change.
    pub updated_at: Timestamp,
    /// Items in the thread.
    pub item_count: usize,
    /// The items.
    #[serde(default)]
    pub items: Vec<Item>,
    /// Everything pointed at so far.
    #[serde(default)]
    pub mentions: Vec<Mention>,
    /// Model spend of this thread, micro-US-dollars.
    #[serde(default)]
    pub spend_usd_micro: u64,
    /// Input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,
}

impl Thread {
    /// The summary form (no items).
    #[must_use]
    pub fn summary(&self) -> Value {
        serde_json::json!({
            "thread_id": self.thread_id,
            "title": self.title,
            "state": self.state,
            "access": self.access,
            "model": self.model,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "item_count": self.item_count,
            "spend_usd_micro": self.spend_usd_micro,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
        })
    }
}

/// A tool call the model made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for the call.
    pub id: String,
    /// The tool, in the dotted form of the table.
    pub name: String,
    /// The arguments.
    pub args: Value,
}

/// What the model sees: the transcript, separate from what the interface draws.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Msg {
    /// A person's message.
    User {
        /// Text.
        text: String,
    },
    /// The model's answer: words and tool calls.
    Assistant {
        /// Text.
        text: String,
        /// Calls.
        tool_calls: Vec<ToolCall>,
    },
    /// What a tool returned. Untrusted data.
    ToolResult {
        /// The call it answers.
        call_id: String,
        /// The tool.
        name: String,
        /// The (bounded) result text.
        text: String,
        /// The call failed or was refused.
        is_error: bool,
    },
}

/// The agent's settings (O-17 section 5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentSettings {
    /// Standing instructions for every thread.
    pub instructions: String,
    /// The access a new thread starts with.
    pub default_access: String,
    /// The provider a new thread starts with.
    pub default_model: String,
    /// Model spend allowed per thread, US dollars; 0 means no model call.
    pub spend_cap_usd: f64,
    /// Model spend allowed per UTC day across threads, US dollars; 0 means no model call.
    pub day_cap_usd: f64,
    /// The tools the agent may call at all.
    pub tools_enabled: Vec<String>,
    /// Finished threads are removed after this many days.
    pub retention_days: u32,
    /// No model call at all, whatever else is set.
    pub offline: bool,
    /// Providers the person agreed to send a thread's text to. Empty until they tick one.
    pub consented_providers: Vec<String>,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            instructions: String::new(),
            default_access: "read".into(),
            default_model: String::new(),
            spend_cap_usd: 1.0,
            day_cap_usd: 5.0,
            tools_enabled: super::tools::table()
                .iter()
                .map(|t| t.name.to_owned())
                .collect(),
            retention_days: 30,
            offline: false,
            consented_providers: Vec::new(),
        }
    }
}
