//! The ladder configuration, `<state_dir>/llm.json` (O-03 section 8.1).
//!
//! An ordered list of providers. The first enabled provider with the capability an operator needs is tried
//! first; later ones are fallbacks for transient failures. The file names API keys only by secret name
//! (`api_key_secret`); the values live in Ogun's secret store and reach a model operator only on its stdin.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::{Host, Url};

use crate::error::LlmError;

/// File name of the ladder configuration inside the state directory.
pub const LADDER_FILE: &str = "llm.json";

/// The only `version` this build reads.
pub const LADDER_VERSION: u32 = 1;

/// Largest accepted `llm.json`.
pub const MAX_LADDER_BYTES: usize = 1024 * 1024;

/// At most this many providers.
pub const MAX_PROVIDERS: usize = 32;

/// Default request timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// The Anthropic API version this build speaks when a provider sets none.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2026-10-21";

/// The provider API family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions and Embeddings.
    Openai,
    /// An OpenAI-compatible endpoint: vLLM, llama.cpp server, LM Studio, OpenRouter and similar.
    OpenaiCompatible,
    /// Ollama `/api/chat` and `/api/embed`.
    Ollama,
    /// Google Gemini `generateContent` and `batchEmbedContents`.
    Google,
}

impl ProviderKind {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::OpenaiCompatible => "openai_compatible",
            Self::Ollama => "ollama",
            Self::Google => "google",
        }
    }

    /// The base URL used when a provider sets none. `openai_compatible` has no default.
    #[must_use]
    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com"),
            Self::Openai => Some("https://api.openai.com/v1"),
            Self::OpenaiCompatible => None,
            Self::Ollama => Some("http://127.0.0.1:11434"),
            Self::Google => Some("https://generativelanguage.googleapis.com/v1beta"),
        }
    }

    /// Whether an enabled provider of this kind must name an API key.
    #[must_use]
    pub const fn requires_key(self) -> bool {
        matches!(self, Self::Anthropic | Self::Openai | Self::Google)
    }

    /// Capabilities assumed when a provider lists none.
    #[must_use]
    pub fn default_capabilities(self) -> &'static [Capability] {
        match self {
            Self::Anthropic => &[Capability::Chat, Capability::JsonSchema],
            Self::Openai | Self::Ollama | Self::Google => &[
                Capability::Chat,
                Capability::JsonSchema,
                Capability::Embeddings,
            ],
            Self::OpenaiCompatible => &[Capability::Chat],
        }
    }

    /// Capabilities this build can drive for the kind.
    #[must_use]
    pub fn supported_capabilities(self) -> &'static [Capability] {
        match self {
            Self::Anthropic => &[Capability::Chat, Capability::JsonSchema],
            _ => &[
                Capability::Chat,
                Capability::JsonSchema,
                Capability::Embeddings,
            ],
        }
    }
}

/// What a provider entry can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Text generation.
    Chat,
    /// Native JSON-schema-constrained output. Without it, structured output uses an instruction plus local
    /// validation and one repair attempt.
    JsonSchema,
    /// Embedding vectors.
    Embeddings,
}

impl Capability {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::JsonSchema => "json_schema",
            Self::Embeddings => "embeddings",
        }
    }
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// One rung of the ladder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Unique name, `^[a-z][a-z0-9_-]{0,63}$`. Appears in usage lines, the ledger and errors.
    pub name: String,
    /// API family.
    pub kind: ProviderKind,
    /// Disabled entries are kept for the user to fill in and are never called.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Endpoint base; the kind's default when absent (`openai_compatible` has none). `https`, or `http` to a
    /// loopback host; plain `http` elsewhere needs `allow_insecure_http` and no API key. No user name,
    /// password, query or fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Model id, as the provider spells it. May be empty only while the entry is disabled.
    pub model: String,
    /// Name of the secret holding the API key. Never the key itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_secret: Option<String>,
    /// Price of one million input tokens, in micro-US-dollars.
    pub input_per_mtok_usd_micro: u64,
    /// Price of one million output tokens (including reasoning tokens), in micro-US-dollars.
    pub output_per_mtok_usd_micro: u64,
    /// Upper bound for the output of one call; also the worst case the budget check assumes.
    pub max_output_tokens: u32,
    /// Request timeout.
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// What the entry may be used for; the kind's defaults when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<Capability>>,
    /// Anthropic only: the `anthropic-version` header ([`DEFAULT_ANTHROPIC_VERSION`] when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    /// Allows plain `http` to a non-loopback host (a LAN inference server). Never together with an API key.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_insecure_http: bool,
}

impl ProviderConfig {
    /// The effective capabilities.
    #[must_use]
    pub fn capabilities(&self) -> Vec<Capability> {
        match &self.capabilities {
            Some(c) => c.clone(),
            None => self.kind.default_capabilities().to_vec(),
        }
    }

    /// Whether the entry has the capability.
    #[must_use]
    pub fn has(&self, c: Capability) -> bool {
        self.capabilities().contains(&c)
    }

    /// The effective base URL, parsed. Fails for an `openai_compatible` entry without one.
    pub fn base(&self) -> Result<Url, LlmError> {
        let raw = match (&self.base_url, self.kind.default_base_url()) {
            (Some(s), _) => s.as_str(),
            (None, Some(d)) => d,
            (None, None) => {
                return Err(LlmError::config(
                    format!("providers.{}.base_url", self.name),
                    "an openai_compatible provider needs a base_url",
                ));
            }
        };
        Url::parse(raw.trim_end_matches('/')).map_err(|_| {
            LlmError::config(
                format!("providers.{}.base_url", self.name),
                "not an absolute URL",
            )
        })
    }

    /// The host the provider is reached on, for a sandbox network allowlist.
    pub fn host(&self) -> Result<String, LlmError> {
        let base = self.base()?;
        let host = base.host_str().unwrap_or_default().to_owned();
        Ok(match base.port() {
            Some(p) => format!("{host}:{p}"),
            None => host,
        })
    }

    fn validate(&self, at: &str) -> Result<(), LlmError> {
        let field = |f: &str| format!("{at}.{f}");
        if !valid_name(&self.name) {
            return Err(LlmError::config(
                field("name"),
                "must match ^[a-z][a-z0-9_-]{0,63}$",
            ));
        }
        if let Some(secret) = &self.api_key_secret
            && !valid_secret_name(secret)
        {
            return Err(LlmError::config(
                field("api_key_secret"),
                "a secret name: 1 to 128 of A-Z a-z 0-9 . _ -",
            ));
        }
        if !(1_000..=600_000).contains(&self.timeout_ms) {
            return Err(LlmError::config(
                field("timeout_ms"),
                "between 1000 and 600000",
            ));
        }
        if !(1..=1_000_000).contains(&self.max_output_tokens) {
            return Err(LlmError::config(
                field("max_output_tokens"),
                "between 1 and 1000000",
            ));
        }
        if let Some(caps) = &self.capabilities {
            if caps.is_empty() {
                return Err(LlmError::config(
                    field("capabilities"),
                    "list at least one capability or leave the member out",
                ));
            }
            let set: BTreeSet<_> = caps.iter().collect();
            if set.len() != caps.len() {
                return Err(LlmError::config(
                    field("capabilities"),
                    "duplicate capability",
                ));
            }
            for c in caps {
                if !self.kind.supported_capabilities().contains(c) {
                    return Err(LlmError::config(
                        field("capabilities"),
                        format!(
                            "{} providers do not offer {}",
                            self.kind.as_str(),
                            c.as_str()
                        ),
                    ));
                }
            }
            if caps.contains(&Capability::JsonSchema) && !caps.contains(&Capability::Chat) {
                return Err(LlmError::config(
                    field("capabilities"),
                    "json_schema needs chat",
                ));
            }
        }
        if let Some(v) = &self.api_version {
            if self.kind != ProviderKind::Anthropic {
                return Err(LlmError::config(
                    field("api_version"),
                    "only anthropic providers take an api_version",
                ));
            }
            if v.is_empty()
                || v.len() > 32
                || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(LlmError::config(
                    field("api_version"),
                    "1 to 32 of a-z 0-9 -",
                ));
            }
        }
        if !self.enabled {
            // A disabled entry may be incomplete; it is checked when the user enables it.
            return Ok(());
        }
        if !valid_model(&self.model) {
            return Err(LlmError::config(
                field("model"),
                "1 to 200 of A-Z a-z 0-9 . _ : / @ + -",
            ));
        }
        if self.kind.requires_key() && self.api_key_secret.is_none() {
            return Err(LlmError::config(
                field("api_key_secret"),
                format!("{} providers need an API key secret", self.kind.as_str()),
            ));
        }
        let base = self.base().map_err(|_| {
            LlmError::config(
                field("base_url"),
                match self.kind {
                    ProviderKind::OpenaiCompatible => {
                        "an openai_compatible provider needs an absolute base_url"
                    }
                    _ => "not an absolute URL",
                },
            )
        })?;
        if !base.username().is_empty() || base.password().is_some() {
            return Err(LlmError::config(
                field("base_url"),
                "no user name or password in the URL; use api_key_secret",
            ));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(LlmError::config(
                field("base_url"),
                "no query or fragment in the URL",
            ));
        }
        match base.scheme() {
            "https" => {}
            "http" if is_loopback(&base) => {}
            "http" if self.allow_insecure_http => {
                if self.api_key_secret.is_some() {
                    return Err(LlmError::config(
                        field("allow_insecure_http"),
                        "an API key is never sent over plain http to another host",
                    ));
                }
            }
            "http" => {
                return Err(LlmError::config(
                    field("base_url"),
                    "plain http only to a loopback host, or with allow_insecure_http",
                ));
            }
            _ => {
                return Err(LlmError::config(
                    field("base_url"),
                    "the scheme must be https or http",
                ));
            }
        }
        if base.host_str().is_none_or(str::is_empty) {
            return Err(LlmError::config(field("base_url"), "no host"));
        }
        Ok(())
    }
}

/// Whether the URL's host is a loopback address or `localhost`.
#[must_use]
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

fn valid_secret_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn valid_model(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'/' | b'@' | b'+' | b'-')
        })
}

/// The whole `llm.json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LadderConfig {
    /// Always [`LADDER_VERSION`].
    pub version: u32,
    /// The rungs, in order of preference.
    pub providers: Vec<ProviderConfig>,
}

impl Default for LadderConfig {
    /// An empty ladder: no model access.
    fn default() -> Self {
        Self {
            version: LADDER_VERSION,
            providers: Vec::new(),
        }
    }
}

impl LadderConfig {
    /// The path of `llm.json` in a state directory.
    #[must_use]
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join(LADDER_FILE)
    }

    /// Reads and validates `<state_dir>/llm.json`. A missing file is `Ok(None)`: no model access.
    pub fn load(state_dir: &Path) -> Result<Option<Self>, LlmError> {
        let path = Self::path(state_dir);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(LlmError::config(
                    LADDER_FILE,
                    format!("cannot read: {}", e.kind()),
                ));
            }
        };
        let text =
            std::str::from_utf8(&bytes).map_err(|_| LlmError::config(LADDER_FILE, "not UTF-8"))?;
        Self::from_json_str(text).map(Some)
    }

    /// Parses and validates a document.
    pub fn from_json_str(text: &str) -> Result<Self, LlmError> {
        if text.len() > MAX_LADDER_BYTES {
            return Err(LlmError::config(LADDER_FILE, "larger than 1 MiB"));
        }
        zeus_verify::canonical::reject_duplicate_keys(text)
            .map_err(|_| LlmError::config(LADDER_FILE, "duplicate member names"))?;
        let cfg: Self = serde_json::from_str(text).map_err(|e| {
            // serde's message names the member and the expected type, never a value.
            LlmError::config(
                LADDER_FILE,
                format!("line {} column {}: {}", e.line(), e.column(), category(&e)),
            )
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parses and validates the `config.ladder` member a runner passed to a model operator.
    pub fn from_value(value: &Value) -> Result<Self, LlmError> {
        let cfg: Self = serde_json::from_value(value.clone())
            .map_err(|e| LlmError::config("config.ladder", category(&e)))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Checks every rule of O-03 section 8.1.
    pub fn validate(&self) -> Result<(), LlmError> {
        if self.version != LADDER_VERSION {
            return Err(LlmError::config(
                "version",
                format!("this build reads version {LADDER_VERSION}"),
            ));
        }
        if self.providers.len() > MAX_PROVIDERS {
            return Err(LlmError::config(
                "providers",
                format!("at most {MAX_PROVIDERS} providers"),
            ));
        }
        let mut names = BTreeSet::new();
        for (i, p) in self.providers.iter().enumerate() {
            let at = format!("providers[{i}]");
            p.validate(&at)?;
            if !names.insert(p.name.as_str()) {
                return Err(LlmError::config(
                    format!("{at}.name"),
                    "provider names must be unique",
                ));
            }
        }
        Ok(())
    }

    /// Writes the document to `<state_dir>/llm.json` (pretty-printed, atomically through a temporary file).
    pub fn save(&self, state_dir: &Path) -> Result<(), LlmError> {
        self.validate()?;
        let path = Self::path(state_dir);
        let tmp = state_dir.join(format!("{LADDER_FILE}.tmp"));
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|_| LlmError::Internal("cannot serialise the ladder".into()))?;
        text.push('\n');
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| LlmError::config(LADDER_FILE, format!("cannot write: {}", e.kind())))
    }

    /// Enabled providers with the capability, in ladder order. `only`, when given, keeps just the named
    /// providers (still in ladder order); an unknown name is an error.
    pub fn select(
        &self,
        capability: Capability,
        only: Option<&[String]>,
    ) -> Result<Vec<&ProviderConfig>, LlmError> {
        if let Some(names) = only {
            for n in names {
                if !self.providers.iter().any(|p| &p.name == n) {
                    return Err(LlmError::config(
                        "config.providers",
                        "names a provider the ladder does not have",
                    ));
                }
            }
        }
        Ok(self
            .providers
            .iter()
            .filter(|p| p.enabled && p.has(capability))
            .filter(|p| only.is_none_or(|names| names.iter().any(|n| n == &p.name)))
            .collect())
    }

    /// The secret names of the enabled providers. A runner resolves these and passes the values in the start
    /// line's `secrets` map.
    #[must_use]
    pub fn secret_names(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .providers
            .iter()
            .filter(|p| p.enabled)
            .filter_map(|p| p.api_key_secret.as_deref())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The `host[:port]` of every enabled provider, for the sandbox's network allowlist.
    #[must_use]
    pub fn hosts(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .providers
            .iter()
            .filter(|p| p.enabled)
            .filter_map(|p| p.host().ok())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The ladder with only the providers a node may use (its `config.providers`, when set).
    pub fn narrowed(&self, only: Option<&[String]>) -> Result<Self, LlmError> {
        let mut out = self.clone();
        if let Some(names) = only {
            // Reuse the unknown-name check.
            self.select(Capability::Chat, Some(names))?;
            out.providers.retain(|p| names.iter().any(|n| n == &p.name));
        }
        Ok(out)
    }

    /// What a runner does before starting `ogun-op-llm` (O-03 section 8.2): puts the ladder in
    /// `config.ladder` and the attempt's model budget in `config.llm_budget_usd_micro`, replacing members of
    /// those names. `config` must be an object.
    pub fn inject_into(
        &self,
        config: &mut Value,
        llm_budget_usd_micro: u64,
    ) -> Result<(), LlmError> {
        let Value::Object(map) = config else {
            return Err(LlmError::config("config", "must be an object"));
        };
        let ladder = serde_json::to_value(self)
            .map_err(|_| LlmError::Internal("cannot serialise the ladder".into()))?;
        map.insert("ladder".into(), ladder);
        map.insert(
            "llm_budget_usd_micro".into(),
            Value::from(llm_budget_usd_micro),
        );
        Ok(())
    }

    /// A starting point for `llm.json`: the current Anthropic models (enabled, needing the secret
    /// `anthropic_api_key`), and disabled entries for the other kinds with the model left for the user.
    /// Prices are Anthropic's first-party list prices; check them before relying on budgets.
    #[must_use]
    pub fn template() -> Self {
        let anthropic = |name: &str, model: &str, input: u64, output: u64| ProviderConfig {
            name: name.into(),
            kind: ProviderKind::Anthropic,
            enabled: true,
            base_url: Some("https://api.anthropic.com".into()),
            model: model.into(),
            api_key_secret: Some("anthropic_api_key".into()),
            input_per_mtok_usd_micro: input,
            output_per_mtok_usd_micro: output,
            max_output_tokens: 16_000,
            timeout_ms: 300_000,
            capabilities: None,
            api_version: None,
            allow_insecure_http: false,
        };
        let blank = |name: &str, kind: ProviderKind, key: Option<&str>| ProviderConfig {
            name: name.into(),
            kind,
            enabled: false,
            base_url: kind.default_base_url().map(str::to_owned).or_else(|| {
                (kind == ProviderKind::OpenaiCompatible).then(|| "http://127.0.0.1:8000/v1".into())
            }),
            model: String::new(),
            api_key_secret: key.map(str::to_owned),
            input_per_mtok_usd_micro: 0,
            output_per_mtok_usd_micro: 0,
            max_output_tokens: 8_192,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            capabilities: None,
            api_version: None,
            allow_insecure_http: false,
        };
        Self {
            version: LADDER_VERSION,
            providers: vec![
                // Claude Opus 5.5 first; the cheaper models catch 429s and outages, and a person can
                // reorder the ladder for bulk work.
                anthropic("anthropic-opus", "claude-opus-5-5", 4_000_000, 20_000_000),
                anthropic(
                    "anthropic-sonnet",
                    "claude-sonnet-5-5",
                    2_000_000,
                    10_000_000,
                ),
                anthropic("anthropic-haiku", "claude-haiku-5-5", 100_000, 500_000),
                blank("openai", ProviderKind::Openai, Some("openai_api_key")),
                blank("google", ProviderKind::Google, Some("google_api_key")),
                blank("ollama", ProviderKind::Ollama, None),
                blank("local-openai", ProviderKind::OpenaiCompatible, None),
            ],
        }
    }
}

/// A serde error reduced to its category. serde quotes offending values in some messages ("invalid type:
/// string \"...\""); a misplaced API key must not reach an error, so values are cut and only member names and
/// expectations are kept.
pub(crate) fn category(e: &serde_json::Error) -> String {
    let msg = e.to_string();
    let msg = msg.split(" at line ").next().unwrap_or("invalid");
    let expected = |m: &str| {
        m.rsplit_once(", expected ")
            .map(|(_, exp)| format!("expected {exp}"))
    };
    let out = if msg.starts_with("invalid type")
        || msg.starts_with("invalid value")
        || msg.starts_with("invalid length")
        || msg.starts_with("unknown variant")
    {
        match expected(msg) {
            Some(exp) => format!("invalid value, {exp}"),
            None => "invalid value".to_owned(),
        }
    } else if msg.starts_with("unknown field")
        || msg.starts_with("missing field")
        || msg.starts_with("duplicate field")
    {
        msg.split(", expected").next().unwrap_or(msg).to_owned()
    } else {
        "invalid document".to_owned()
    };
    if out.len() > 300 {
        "invalid document".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_valid_and_round_trips() {
        let t = LadderConfig::template();
        t.validate().unwrap();
        let text = serde_json::to_string(&t).unwrap();
        assert_eq!(LadderConfig::from_json_str(&text).unwrap(), t);
        assert_eq!(t.secret_names(), vec!["anthropic_api_key"]);
        assert_eq!(t.hosts(), vec!["api.anthropic.com".to_owned()]);
    }

    #[test]
    fn rejects_bad_urls_and_names() {
        let mut t = LadderConfig::template();
        t.providers[0].base_url = Some("https://user:pw@api.anthropic.com".into());
        assert!(t.validate().is_err());
        t.providers[0].base_url = Some("https://api.anthropic.com/?key=x".into());
        assert!(t.validate().is_err());
        t.providers[0].base_url = Some("http://api.anthropic.com".into());
        assert!(t.validate().is_err());
        t.providers[0].base_url = Some("http://127.0.0.1:9".into());
        t.validate().unwrap();
        t.providers[1].name = "anthropic-opus".into();
        assert!(t.validate().is_err());
    }

    #[test]
    fn insecure_http_never_carries_a_key() {
        let mut t = LadderConfig::default();
        let mut p = LadderConfig::template().providers[6].clone();
        p.enabled = true;
        p.model = "qwen".into();
        p.base_url = Some("http://192.168.1.20:8000/v1".into());
        t.providers.push(p.clone());
        assert!(t.validate().is_err());
        t.providers[0].allow_insecure_http = true;
        t.validate().unwrap();
        t.providers[0].api_key_secret = Some("k".into());
        assert!(t.validate().is_err());
    }

    #[test]
    fn floats_and_unknown_members_are_rejected() {
        let t = r#"{"version":1,"providers":[{"name":"a","kind":"ollama","model":"m","input_per_mtok_usd_micro":1.5,"output_per_mtok_usd_micro":0,"max_output_tokens":10}]}"#;
        assert!(LadderConfig::from_json_str(t).is_err());
        let t = r#"{"version":1,"providers":[{"name":"a","kind":"ollama","model":"m","api_key":"x","input_per_mtok_usd_micro":1,"output_per_mtok_usd_micro":0,"max_output_tokens":10}]}"#;
        assert!(LadderConfig::from_json_str(t).is_err());
    }

    #[test]
    fn inject_replaces_members() {
        let mut config = serde_json::json!({"ladder": "x", "labels": ["a", "b"]});
        LadderConfig::template()
            .inject_into(&mut config, 500)
            .unwrap();
        assert_eq!(config["llm_budget_usd_micro"], 500);
        assert_eq!(config["ladder"]["version"], 1);
        assert!(LadderConfig::from_value(&config["ladder"]).is_ok());
    }
}
