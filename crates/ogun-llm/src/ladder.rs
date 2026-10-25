//! The ladder runner (feature `http`): budget admission, fallback down the ladder and structured output with
//! one repair attempt (O-03 sections 8.7 and 8.8).

use serde_json::Value;

use crate::budget::CallBudget;
use crate::config::{Capability, LadderConfig};
use crate::error::LlmError;
use crate::provider::Provider;
use crate::request::{
    ChatRequest, ChatResponse, EmbedRequest, EmbedResponse, Finish, Message, Usage,
};
use crate::schema::{self, Schema};
use crate::secrets::KeyResolver;

/// The usable rungs for one capability, in ladder order.
#[derive(Debug)]
pub struct Ladder {
    rungs: Vec<Provider>,
    fallback: bool,
    repair: bool,
}

/// A validated structured answer.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonAnswer {
    /// The value; it satisfies the schema and the caller's extra check.
    pub value: Value,
    /// Provider that produced it.
    pub provider: String,
    /// Model that produced it.
    pub model: String,
}

impl Ladder {
    /// The enabled rungs of `cfg` with `capability` (narrowed to `only` when given) whose API key resolves.
    /// A rung whose key is missing is skipped. No rung left is [`LlmError::NotConfigured`].
    pub fn build(
        cfg: &LadderConfig,
        keys: &dyn KeyResolver,
        capability: Capability,
        only: Option<&[String]>,
    ) -> Result<Self, LlmError> {
        cfg.validate()?;
        let mut rungs = Vec::new();
        for p in cfg.select(capability, only)? {
            let key = p.api_key_secret.as_deref().and_then(|n| keys.resolve(n));
            if p.api_key_secret.is_some() && key.is_none() {
                tracing::warn!(provider = %p.name, "skipping a provider whose API key secret is not available");
                continue;
            }
            rungs.push(Provider::new(p.clone(), key)?);
        }
        if rungs.is_empty() {
            return Err(LlmError::NotConfigured(format!(
                "no enabled provider offers {} with its API key available",
                capability.as_str()
            )));
        }
        Ok(Self {
            rungs,
            fallback: true,
            repair: true,
        })
    }

    /// A ladder over prepared providers (tests, embedding callers).
    #[must_use]
    pub fn from_providers(rungs: Vec<Provider>) -> Self {
        Self {
            rungs,
            fallback: true,
            repair: true,
        }
    }

    /// Whether transient failures move on to the next rung (default on; the benchmark profile turns it off).
    #[must_use]
    pub fn with_fallback(mut self, on: bool) -> Self {
        self.fallback = on;
        self
    }

    /// Whether an invalid structured answer gets one repair request (default on).
    #[must_use]
    pub fn with_repair(mut self, on: bool) -> Self {
        self.repair = on;
        self
    }

    /// The rungs.
    #[must_use]
    pub fn rungs(&self) -> &[Provider] {
        &self.rungs
    }

    /// The largest output limit any rung allows.
    #[must_use]
    pub fn max_output_tokens(&self) -> u32 {
        self.rungs
            .iter()
            .map(|p| p.config().max_output_tokens)
            .max()
            .unwrap_or(1)
    }

    async fn call(
        p: &Provider,
        req: &ChatRequest,
        budget: &mut CallBudget,
        on_usage: &mut dyn FnMut(&Usage),
    ) -> Result<ChatResponse, LlmError> {
        let bound = p.worst_case_chat(req);
        budget.admit(bound)?;
        let resp = match p.chat(req).await {
            Ok(resp) => resp,
            Err(error) => {
                Self::record_failed_call(p, &error, bound, budget, on_usage);
                return Err(error);
            }
        };
        budget.record(resp.usage.cost_usd_micro);
        on_usage(&resp.usage);
        Ok(resp)
    }

    // A timeout, lost/malformed response or model refusal can still have been billed. Account
    // for its admitted bound before trying another provider, and report it to the daemon so
    // a successful fallback cannot refund the unknown first bill. Rejected requests are free.
    fn record_failed_call(
        p: &Provider,
        error: &LlmError,
        bound: u64,
        budget: &mut CallBudget,
        on_usage: &mut dyn FnMut(&Usage),
    ) {
        if matches!(
            error,
            LlmError::NotConfigured(_)
                | LlmError::ConfigInvalid { .. }
                | LlmError::Throttled { .. }
                | LlmError::AuthFailed { .. }
                | LlmError::Rejected { .. }
        ) {
            return;
        }
        budget.record(bound);
        on_usage(&Usage {
            provider: p.name().to_owned(),
            model: p.config().model.clone(),
            // No token counts were returned; the monetary charge is a conservative bound.
            input_tokens: 0,
            output_tokens: 0,
            cost_usd_micro: bound,
        });
    }

    fn eligible(&self) -> impl Iterator<Item = (usize, &Provider)> {
        let n = if self.fallback { self.rungs.len() } else { 1 };
        self.rungs.iter().enumerate().take(n)
    }

    /// One free-text chat call, falling back on transient failures.
    pub async fn chat(
        &self,
        req: &ChatRequest,
        budget: &mut CallBudget,
        on_usage: &mut dyn FnMut(&Usage),
    ) -> Result<ChatResponse, LlmError> {
        let mut last = None;
        for (_, p) in self.eligible() {
            match Self::call(p, req, budget, on_usage).await {
                Ok(r) => return Ok(r),
                Err(e) if e.falls_back() => {
                    tracing::warn!(
                        provider = p.name(),
                        code = e.code(),
                        "provider failed; trying the next rung"
                    );
                    last = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| LlmError::NotConfigured("the ladder is empty".into())))
    }

    /// A structured call: the answer must parse as JSON, satisfy `schema` and pass `check`. An answer that
    /// does not gets one repair request on the same rung (unless repair is off); a second failure is
    /// [`LlmError::OutputInvalid`]. Transient failures fall back to the next rung, which starts afresh.
    pub async fn chat_json(
        &self,
        req: &ChatRequest,
        schema: &Schema,
        check: &dyn Fn(&Value) -> Result<(), String>,
        budget: &mut CallBudget,
        on_usage: &mut dyn FnMut(&Usage),
    ) -> Result<JsonAnswer, LlmError> {
        let mut req = req.clone();
        req.schema = Some(schema.value().clone());
        let mut last = None;
        'rungs: for (_, p) in self.eligible() {
            let mut conversation = req.clone();
            let attempts = if self.repair { 2 } else { 1 };
            let mut problems = String::new();
            for attempt in 0..attempts {
                let resp = match Self::call(p, &conversation, budget, on_usage).await {
                    Ok(r) => r,
                    Err(e) if e.falls_back() => {
                        tracing::warn!(
                            provider = p.name(),
                            code = e.code(),
                            "provider failed; trying the next rung"
                        );
                        last = Some(e);
                        continue 'rungs;
                    }
                    Err(e) => return Err(e),
                };
                match judge(&resp, schema, check) {
                    Ok(value) => {
                        return Ok(JsonAnswer {
                            value,
                            provider: resp.usage.provider,
                            model: resp.usage.model,
                        });
                    }
                    Err(why) => {
                        tracing::info!(
                            provider = p.name(),
                            attempt,
                            "structured answer did not validate"
                        );
                        problems = why;
                        conversation.messages.push(Message::assistant(resp.text));
                        conversation
                            .messages
                            .push(Message::user(schema::repair_message(&problems)));
                    }
                }
            }
            return Err(LlmError::OutputInvalid { reason: problems });
        }
        Err(last.unwrap_or_else(|| LlmError::NotConfigured("the ladder is empty".into())))
    }

    /// One embeddings call. Vectors from different models are not comparable, so once a call has succeeded
    /// on a rung (`pinned`), later calls use only that rung and never fall back.
    pub async fn embed(
        &self,
        req: &EmbedRequest,
        pinned: &mut Option<usize>,
        budget: &mut CallBudget,
        on_usage: &mut dyn FnMut(&Usage),
    ) -> Result<EmbedResponse, LlmError> {
        let candidates: Vec<(usize, &Provider)> = match *pinned {
            Some(i) => self.rungs.get(i).map(|p| (i, p)).into_iter().collect(),
            None => self.eligible().collect(),
        };
        let mut last = None;
        for (i, p) in candidates {
            let bound = p.worst_case_embed(req);
            budget.admit(bound)?;
            match p.embed(req).await {
                Ok(resp) => {
                    budget.record(resp.usage.cost_usd_micro);
                    on_usage(&resp.usage);
                    *pinned = Some(i);
                    return Ok(resp);
                }
                Err(e) => {
                    Self::record_failed_call(p, &e, bound, budget, on_usage);
                    if e.falls_back() && pinned.is_none() {
                        tracing::warn!(
                            provider = p.name(),
                            code = e.code(),
                            "provider failed; trying the next rung"
                        );
                        last = Some(e);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| LlmError::NotConfigured("the ladder is empty".into())))
    }
}

/// Parses and validates an answer, describing what is wrong without quoting it.
fn judge(
    resp: &ChatResponse,
    schema: &Schema,
    check: &dyn Fn(&Value) -> Result<(), String>,
) -> Result<Value, String> {
    let cut = if resp.finish == Finish::Length {
        " (the reply was cut off at the output limit)"
    } else {
        ""
    };
    let value =
        schema::parse_answer(&resp.text).ok_or_else(|| format!("the reply is not JSON{cut}"))?;
    schema.check(&value).map_err(|p| format!("{p}{cut}"))?;
    check(&value)?;
    Ok(value)
}
