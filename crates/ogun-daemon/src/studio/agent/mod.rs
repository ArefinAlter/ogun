//! Agent threads (ADR 0014, O-17): daemon objects over a closed tool table.
//!
//! * A **thread** is stored in the state directory (`settings/agent-threads/<id>.json`), streams to every Studio
//!   that has it open (long-polling `agent.threads.get`), and is private to the person who made it.
//! * The **loop** runs here, in the daemon (O-17 open issue 1): provider keys never reach a webview, a prompt or
//!   a log, and the spend ledger (O-03 section 8.9) is the daemon's own.
//! * The **tools** are the closed table of [`tools`]; each call goes through the policy table as an `agent`
//!   client. No tool decides an approval, reads a secret value, runs a shell command or reaches a host.
//! * **Cards** are built by the daemon from tool results; a person presses them (`agent.act`), and the daemon
//!   performs the action with the caller's own role. `agent.act` is refused to an `agent` credential.
//! * **Consent and caps.** Nothing is sent to a provider until the person ticks it (`consented_providers`), a
//!   global offline switch overrides everything, and a spend cap of zero means no model call.
//! * **Secrets.** Standing instructions and every message are checked against the redaction rules and **refused**,
//!   not masked, when they look like a secret.

pub mod provider;
pub mod tools;
mod turn;
pub mod types;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ogun_api::{ApiError, ApiResult};
use ogun_llm::config::{LadderConfig, ProviderConfig};
use ogun_llm::ledger::{Ledger, UsageRecord};
use ogun_types::{CredentialId, IdGenerator, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use zeroize::Zeroizing;

use self::types::{AgentSettings, Item, ItemBody, Mention, Msg, Thread, rank};
use super::{Docs, args, out};
use crate::redact::looks_like_secret;
use crate::server::Conn;
use crate::state::{Daemon, lock};

/// Longest message a person may send.
pub(crate) const MAX_MESSAGE: usize = 16_000;
/// Most mentions of one message.
pub(crate) const MAX_MENTIONS: usize = 12;
/// Longest standing instructions.
const MAX_INSTRUCTIONS: usize = 8000;
/// Sends per minute to one thread.
const SENDS_PER_MINUTE: usize = 20;
/// Items a thread keeps.
pub(crate) const MAX_ITEMS: usize = 600;
/// Threads kept.
const MAX_THREADS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ThreadFile {
    pub thread: Thread,
    #[serde(default)]
    pub transcript: Vec<Msg>,
    #[serde(default)]
    pub rev: u64,
    /// This thread's provider cannot call tools natively; it uses the JSON protocol.
    #[serde(default)]
    pub json_tools: bool,
}

pub(crate) struct Live {
    pub data: Mutex<ThreadFile>,
    pub stop: watch::Sender<bool>,
    pub running: std::sync::atomic::AtomicBool,
}

/// The agent's state.
pub struct Agents {
    docs: Docs,
    dir: PathBuf,
    state_dir: PathBuf,
    settings: Mutex<AgentSettings>,
    live: Mutex<HashMap<String, Arc<Live>>>,
    tick: watch::Sender<u64>,
    seq: AtomicU64,
    ledger: Mutex<Option<Ledger>>,
    ledger_note: Mutex<Option<String>>,
    sends: Mutex<HashMap<String, VecDeque<Instant>>>,
    credential: CredentialId,
}

impl std::fmt::Debug for Agents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agents").finish_non_exhaustive()
    }
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", ulid::Ulid::generate())
}

fn title_of(text: &str) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return "New thread".into();
    }
    if t.chars().count() <= 56 {
        return t;
    }
    let cut: String = t.chars().take(56).collect();
    let at = cut.rfind(' ').filter(|a| *a > 30).unwrap_or(cut.len());
    format!(
        "{}…",
        cut[..at].trim_end_matches(['.', ',', ';', ':', '!', '?'])
    )
}

impl Agents {
    /// Loads settings and threads from the state directory.
    ///
    /// # Errors
    ///
    /// `api.unavailable`.
    pub fn open(state_dir: &Path, docs: &Docs) -> ApiResult<Self> {
        let dir = docs.dir().join("agent-threads");
        crate::creds::private_dir(&dir)?;
        let settings: AgentSettings = docs.read("agent");
        let ids = IdGenerator::new();
        let me = Self {
            docs: docs.clone(),
            dir,
            state_dir: state_dir.to_owned(),
            settings: Mutex::new(settings),
            live: Mutex::new(HashMap::new()),
            tick: watch::Sender::new(1),
            seq: AtomicU64::new(1),
            ledger: Mutex::new(None),
            ledger_note: Mutex::new(None),
            sends: Mutex::new(HashMap::new()),
            credential: ids.credential_id(),
        };
        me.load_threads();
        Ok(me)
    }

    fn load_threads(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let retention =
            Duration::from_secs(u64::from(lock(&self.settings).retention_days) * 86_400);
        let now = Timestamp::now().unix_millis();
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(mut f) = serde_json::from_slice::<ThreadFile>(&bytes) else {
                continue;
            };
            let age = now - f.thread.updated_at.unix_millis();
            if f.thread.state != "waiting"
                && age > i64::try_from(retention.as_millis()).unwrap_or(i64::MAX)
            {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            if f.thread.state == "running" {
                f.thread.state = "idle".into();
                for it in &mut f.thread.items {
                    if let ItemBody::Assistant { streaming, .. } = &mut it.body {
                        *streaming = false;
                    }
                    if let ItemBody::Tool { state, .. } = &mut it.body
                        && state == "running"
                    {
                        "error".clone_into(state);
                    }
                }
                self.push_notice_into(&mut f, "warn", "The engine restarted while the agent was answering. Ask again if you need the rest.");
            }
            let id = f.thread.thread_id.clone();
            let (stop, _) = watch::channel(false);
            lock(&self.live).insert(
                id,
                Arc::new(Live {
                    data: Mutex::new(f),
                    stop,
                    running: std::sync::atomic::AtomicBool::new(false),
                }),
            );
        }
    }

    /// Models changed (the ladder was saved): wake the model pickers.
    pub fn changed_for_models(&self) {
        self.changed();
    }

    pub(crate) fn conn(&self) -> Conn {
        tools::agent_conn(self.credential)
    }

    pub(crate) fn settings(&self) -> AgentSettings {
        lock(&self.settings).clone()
    }

    pub(crate) fn live(&self, id: &str) -> Option<Arc<Live>> {
        lock(&self.live).get(id).cloned()
    }

    /// Tells everyone waiting that something changed.
    pub(crate) fn changed(&self) {
        let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.tick.send_replace(n);
    }

    pub(crate) fn save(&self, f: &ThreadFile) {
        let path = self.dir.join(format!("{}.json", f.thread.thread_id));
        if let Ok(bytes) = serde_json::to_vec(f)
            && let Err(e) = crate::creds::write_private(&path, &bytes)
        {
            tracing::warn!(error = %e, "an agent thread could not be saved");
        }
    }

    fn push_notice_into(&self, f: &mut ThreadFile, tone: &str, text: &str) {
        f.thread.items.push(Item {
            id: new_id("itm"),
            at: Timestamp::now(),
            body: ItemBody::Notice {
                tone: tone.into(),
                text: text.into(),
            },
        });
        f.thread.item_count = f.thread.items.len();
        f.thread.updated_at = Timestamp::now();
        f.rev += 1;
    }

    /// Appends the usage of one provider call to the ledger (opened on first use).
    pub(crate) fn record(&self, usage: UsageRecord) {
        let mut guard = lock(&self.ledger);
        if guard.is_none() {
            match Ledger::open(&self.state_dir) {
                Ok(l) => *guard = Some(l),
                Err(e) => {
                    *lock(&self.ledger_note) = Some(e.to_string());
                    tracing::warn!(error = %e, "the spend ledger could not be opened");
                    return;
                }
            }
        }
        if let Some(l) = guard.as_mut()
            && let Err(e) = l.append(usage)
        {
            tracing::warn!(error = %e, "the spend ledger could not be written");
        }
    }

    fn with_ledger<T>(&self, f: impl FnOnce(&Ledger) -> T) -> Option<T> {
        let mut guard = lock(&self.ledger);
        if guard.is_none() {
            *guard = Ledger::open(&self.state_dir).ok();
        }
        guard.as_ref().map(f)
    }

    /// Spend of one thread, micro-US-dollars.
    pub(crate) fn thread_spend(&self, id: &str) -> u64 {
        self.with_ledger(|l| l.by_thread(id).cost_usd_micro)
            .unwrap_or(0)
    }

    /// Spend on a UTC day, micro-US-dollars.
    pub(crate) fn day_spend(&self, day: &str) -> u64 {
        self.with_ledger(|l| l.by_day(day).cost_usd_micro)
            .unwrap_or(0)
    }

    /// The month's spend and entries, for the meter.
    pub(crate) fn month_spend(&self, month: &str) -> (u64, u64) {
        self.with_ledger(|l| {
            l.days()
                .iter()
                .filter(|(d, _)| d.starts_with(month))
                .fold((0u64, 0u64), |(c, n), (_, s)| {
                    (c + s.cost_usd_micro, n + s.calls)
                })
        })
        .unwrap_or((0, 0))
    }
}

/// Supported local subscription agents.
pub(crate) const SUBSCRIPTION_AGENTS: &[(&str, &str, &str)] = &[
    ("claude_subscription", "claude", "Claude Code (Subscription)"),
    ("codex_subscription", "codex", "Codex (Subscription)"),
    ("antigravity_subscription", "antigravity", "Antigravity (Subscription)"),
    ("cursor_subscription", "cursor", "Cursor (Subscription)"),
    ("grok_subscription", "grok", "Grok Build (Subscription)"),
];

/// Why a thread may not call a model right now, or the provider to use.
pub(crate) fn gate(
    d: &Daemon,
    settings: &AgentSettings,
    model: &str,
    thread: Option<&str>,
) -> Result<(ProviderConfig, Option<Zeroizing<String>>), String> {
    if settings.offline {
        return Err("Offline mode is on in Settings, Agent: no model is called.".into());
    }
    let ladder = LadderConfig::load(&d.paths.state_dir)
        .map_err(|e| format!("The provider list (llm.json) is not valid: {e}"))?;

    let mut cfg_opt = ladder
        .as_ref()
        .and_then(|l| l.providers.iter().find(|p| p.name == model && p.enabled).cloned());

    if cfg_opt.is_none() {
        for (sub_name, agent, _) in SUBSCRIPTION_AGENTS {
            if model == *sub_name || model == *agent {
                if ogun_llm::local_agent::installed(agent) {
                    cfg_opt = Some(ProviderConfig {
                        name: (*sub_name).to_string(),
                        kind: ogun_llm::config::ProviderKind::OpenaiCompatible,
                        enabled: true,
                        base_url: Some(format!("https://{agent}.local-agent.invalid/v1")),
                        model: "default".into(),
                        api_key_secret: None,
                        input_per_mtok_usd_micro: 0,
                        output_per_mtok_usd_micro: 0,
                        max_output_tokens: 8192,
                        timeout_ms: 120_000,
                        capabilities: Some(vec![ogun_llm::config::Capability::Chat]),
                        api_version: None,
                        allow_insecure_http: false,
                    });
                    break;
                }
            }
        }
    }

    let mut cfg = cfg_opt.ok_or_else(|| {
        format!("No enabled provider is named {model}. Pick one in the thread's model menu or add it in Settings, Providers.")
    })?;

    let is_subscription = cfg
        .base_url
        .as_deref()
        .map(|u| u.contains(".local-agent.invalid"))
        .unwrap_or(false);

    if cfg.model.is_empty() {
        if is_subscription {
            cfg.model = "default".into();
        } else {
            return Err(format!("Provider {model} has no model id yet."));
        }
    }

    let key = match &cfg.api_key_secret {
        Some(name) => ogun_ops::Secrets::resolve(d.secrets.as_ref(), name),
        None => None,
    };
    if !is_subscription && cfg.kind.requires_key() && key.is_none() {
        return Err(format!(
            "No API key is stored for {model}. Add it in Settings, Providers."
        ));
    }
    if !is_subscription && !settings.consented_providers.iter().any(|p| p == model) {
        return Err(format!(
            "You have not agreed to send this thread's text to {model}. Tick it in Settings, Agent."
        ));
    }
    if !is_subscription && (settings.spend_cap_usd <= 0.0 || settings.day_cap_usd <= 0.0) {
        return Err("A spend cap of zero means no model call. Raise it in Settings, Agent.".into());
    }
    if let Some(t) = thread {
        let spent = d.studio.agent.thread_spend(t);
        if spent >= usd_micro(settings.spend_cap_usd) {
            return Err("This thread reached its spend cap. Raise the cap in Settings, Agent or start a new thread.".into());
        }
    }
    let day = &Timestamp::now().to_rfc3339()[..10];
    if d.studio.agent.day_spend(day) >= usd_micro(settings.day_cap_usd) {
        return Err("Today's agent spend cap is reached. It resets at midnight UTC, or raise it in Settings, Agent.".into());
    }
    Ok((cfg, key))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn usd_micro(usd: f64) -> u64 {
    (usd * 1_000_000.0).round().max(0.0) as u64
}

fn no_thread() -> ApiError {
    ApiError::new("agent.thread_not_found", "no such thread")
}

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    since: Option<u64>,
    #[serde(default)]
    wait_ms: Option<u64>,
}

#[derive(Deserialize)]
struct Create {
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct Rename {
    thread_id: String,
    title: String,
}

#[derive(Deserialize)]
struct ThreadRef {
    thread_id: String,
}

#[derive(Deserialize)]
struct SetAccess {
    thread_id: String,
    access: String,
}

#[derive(Deserialize)]
struct Send {
    thread_id: String,
    text: String,
    #[serde(default)]
    mentions: Vec<Mention>,
}

#[derive(Deserialize)]
struct Act {
    thread_id: String,
    item_id: String,
    action: String,
    #[serde(default)]
    #[allow(dead_code)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
struct Put {
    patch: serde_json::Map<String, Value>,
    #[serde(default)]
    #[allow(dead_code)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
struct Preview {
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

async fn wait_for(d: &Daemon, since: Option<u64>, wait_ms: Option<u64>, current: impl Fn() -> u64) {
    let Some(since) = since else { return };
    let mut rx = d.studio.agent.tick.subscribe();
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(wait_ms.unwrap_or(0).min(30_000));
    while current() <= since {
        if tokio::time::timeout_at(deadline, rx.changed())
            .await
            .is_err()
        {
            return;
        }
    }
}

fn settings_put(d: &Daemon, patch: serde_json::Map<String, Value>) -> ApiResult<AgentSettings> {
    let agents = &d.studio.agent;
    let mut v =
        serde_json::to_value(agents.settings()).map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(o) = v.as_object_mut() {
        for (k, x) in patch {
            o.insert(k, x);
        }
    }
    let next: AgentSettings = serde_json::from_value(v)
        .map_err(|e| ApiError::invalid_params(format!("agent settings: {e}")))?;
    if next.instructions.chars().count() > MAX_INSTRUCTIONS {
        return Err(ApiError::invalid_params(
            "agent settings: instructions are at most 8000 characters",
        ));
    }
    if looks_like_secret(&next.instructions) {
        return Err(ApiError::new(
            "agent.secret_refused",
            "the instructions look like they contain a secret (a key, a token, a private key or a password). Name a secret instead of writing it; nothing was saved.",
        ));
    }
    if rank(&next.default_access).is_none() {
        return Err(ApiError::invalid_params(
            "agent settings: default_access is read, author or operate",
        ));
    }
    if !(0.0..=1000.0).contains(&next.spend_cap_usd)
        || !(0.0..=10_000.0).contains(&next.day_cap_usd)
    {
        return Err(ApiError::invalid_params(
            "agent settings: caps are 0 to 1000 (per thread) and 0 to 10000 (per day) US dollars",
        ));
    }
    if next.tools_enabled.iter().any(|t| tools::find(t).is_none()) {
        return Err(ApiError::invalid_params(
            "agent settings: tools_enabled names a tool that does not exist",
        ));
    }
    if ![7, 30, 90, 365].contains(&next.retention_days) {
        return Err(ApiError::invalid_params(
            "agent settings: retention_days is 7, 30, 90 or 365",
        ));
    }
    if next
        .consented_providers
        .iter()
        .any(|p| p.len() > 64 || p.chars().any(char::is_control))
    {
        return Err(ApiError::invalid_params(
            "agent settings: consented_providers holds provider names",
        ));
    }
    agents.docs.write("agent", &next)?;
    *lock(&agents.settings) = next.clone();
    agents.changed();
    Ok(next)
}

fn models(d: &Daemon) -> Value {
    let settings = d.studio.agent.settings();
    let mut list = match LadderConfig::load(&d.paths.state_dir) {
        Ok(Some(l)) => l.providers,
        _ => Vec::new(),
    };
    for (name, agent, _) in SUBSCRIPTION_AGENTS {
        let already_present = list.iter().any(|p| {
            p.name == *name
                || p.base_url
                    .as_deref()
                    .unwrap_or("")
                    .contains(&format!("{agent}.local-agent.invalid"))
        });
        if !already_present && ogun_llm::local_agent::installed(agent) {
            list.push(ProviderConfig {
                name: (*name).to_string(),
                kind: ogun_llm::config::ProviderKind::OpenaiCompatible,
                enabled: true,
                base_url: Some(format!("https://{agent}.local-agent.invalid/v1")),
                model: "default".into(),
                api_key_secret: None,
                input_per_mtok_usd_micro: 0,
                output_per_mtok_usd_micro: 0,
                max_output_tokens: 8192,
                timeout_ms: 120_000,
                capabilities: Some(vec![ogun_llm::config::Capability::Chat]),
                api_version: None,
                allow_insecure_http: false,
            });
        }
    }
    let rows: Vec<Value> = list
        .iter()
        .filter(|p| p.enabled && p.has(ogun_llm::config::Capability::Chat))
        .map(|p| {
            let gated = gate(d, &settings, &p.name, None);
            let is_sub = p.base_url.as_deref().unwrap_or("").contains(".local-agent.invalid");
            let label = if is_sub {
                format!("{} · Pro/Team (Subscription)", p.name)
            } else {
                format!("{} · {}", p.name, p.model)
            };
            json!({
                "id": p.name,
                "label": label,
                "provider": p.kind.as_str(),
                "available": gated.is_ok(),
                "note": gated.err(),
            })
        })
        .collect();
    json!(rows)
}

fn tool_rows(settings: &AgentSettings) -> Value {
    json!(
        tools::table()
            .iter()
            .filter(|t| settings.tools_enabled.iter().any(|e| e == t.name) || true)
            .map(|t| json!({ "name": t.name, "access": t.access, "summary": t.summary }))
            .collect::<Vec<_>>()
    )
}

/// Routes `agent.*`.
pub async fn dispatch(
    d: &Arc<Daemon>,
    conn: &Conn,
    method: &str,
    p: Value,
) -> Option<ApiResult<Value>> {
    let agents = &d.studio.agent;
    Some(match method {
        "agent.models" => Ok(models(d)),
        "agent.tools" => Ok(tool_rows(&agents.settings())),
        "agent.settings.get" => out(&agents.settings()),
        "agent.settings.put" => args::<Put>(p).and_then(|s| out(&settings_put(d, s.patch)?)),
        "agent.threads.list" => match args::<Since>(p) {
            Ok(s) => {
                wait_for(d, s.since, s.wait_ms, || agents.seq.load(Ordering::Relaxed)).await;
                let mut rows: Vec<Thread> = lock(&agents.live)
                    .values()
                    .map(|l| lock(&l.data).thread.clone())
                    .collect();
                rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at.unix_millis()));
                Ok(
                    json!({ "rev": agents.seq.load(Ordering::Relaxed), "threads": rows.iter().map(Thread::summary).collect::<Vec<_>>() }),
                )
            }
            Err(e) => Err(e),
        },
        "agent.threads.get" => match args::<Since>(p) {
            Ok(s) => {
                let Some(id) = s.thread_id.clone() else {
                    return Some(Err(ApiError::invalid_params("thread_id is required")));
                };
                let Some(live) = agents.live(&id) else {
                    return Some(Ok(json!({ "rev": 0, "thread": Value::Null })));
                };
                wait_for(d, s.since, s.wait_ms, || lock(&live.data).rev).await;
                let f = lock(&live.data).clone();
                Ok(json!({ "rev": f.rev, "thread": f.thread }))
            }
            Err(e) => Err(e),
        },
        "agent.threads.create" => args::<Create>(p).and_then(|c| {
            let settings = agents.settings();
            let access = c.access.unwrap_or_else(|| settings.default_access.clone());
            if rank(&access).is_none() {
                return Err(ApiError::invalid_params(
                    "access is read, author or operate",
                ));
            }
            if lock(&agents.live).len() >= MAX_THREADS {
                return Err(ApiError::invalid_params("too many threads; delete some"));
            }
            let model = c.model.filter(|m| !m.is_empty()).unwrap_or_else(|| {
                if settings.default_model.is_empty() || settings.default_model == "scripted" {
                    settings
                        .consented_providers
                        .first()
                        .cloned()
                        .unwrap_or_else(|| {
                            for (sub_name, agent, _) in SUBSCRIPTION_AGENTS {
                                if ogun_llm::local_agent::installed(agent) {
                                    return (*sub_name).to_string();
                                }
                            }
                            String::new()
                        })
                } else {
                    settings.default_model.clone()
                }
            });
            let now = Timestamp::now();
            let thread = Thread {
                thread_id: new_id("thr"),
                title: "New thread".into(),
                state: "idle".into(),
                access,
                model,
                created_at: now,
                updated_at: now,
                item_count: 0,
                items: vec![],
                mentions: vec![],
                spend_usd_micro: 0,
                input_tokens: 0,
                output_tokens: 0,
            };
            let f = ThreadFile {
                thread: thread.clone(),
                transcript: vec![],
                rev: 1,
                json_tools: false,
            };
            agents.save(&f);
            let (stop, _) = watch::channel(false);
            lock(&agents.live).insert(
                thread.thread_id.clone(),
                Arc::new(Live {
                    data: Mutex::new(f),
                    stop,
                    running: std::sync::atomic::AtomicBool::new(false),
                }),
            );
            agents.changed();
            out(&thread)
        }),
        "agent.threads.rename" => args::<Rename>(p).and_then(|r| {
            let live = agents.live(&r.thread_id).ok_or_else(no_thread)?;
            let title: String = r
                .title
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect();
            if title.trim().is_empty() {
                return Err(ApiError::invalid_params("a title is not empty"));
            }
            let mut f = lock(&live.data);
            f.thread.title = title;
            f.rev += 1;
            agents.save(&f);
            drop(f);
            agents.changed();
            out(&json!({}))
        }),
        "agent.threads.delete" => args::<ThreadRef>(p).and_then(|r| {
            let live = agents.live(&r.thread_id).ok_or_else(no_thread)?;
            live.stop.send_replace(true);
            lock(&agents.live).remove(&r.thread_id);
            let _ = std::fs::remove_file(agents.dir.join(format!("{}.json", r.thread_id)));
            agents.changed();
            out(&json!({}))
        }),
        "agent.access.set" => args::<SetAccess>(p).and_then(|r| {
            if rank(&r.access).is_none() {
                return Err(ApiError::invalid_params(
                    "access is read, author or operate",
                ));
            }
            let live = agents.live(&r.thread_id).ok_or_else(no_thread)?;
            let mut f = lock(&live.data);
            if f.thread.access != r.access {
                let from = f.thread.access.clone();
                f.thread.access.clone_from(&r.access);
                let text = format!(
                    "Access changed from {from} to {} by {}.",
                    r.access, conn.name
                );
                agents.push_notice_into(&mut f, "info", &text);
                agents.save(&f);
            }
            drop(f);
            agents.changed();
            out(&json!({}))
        }),
        "agent.stop" => args::<ThreadRef>(p).and_then(|r| {
            let live = agents.live(&r.thread_id).ok_or_else(no_thread)?;
            live.stop.send_replace(true);
            out(&json!({}))
        }),
        "agent.send" => {
            args::<Send>(p).and_then(|s| turn::send(d, s.thread_id, s.text, s.mentions))
        }
        "agent.act" => match args::<Act>(p) {
            Ok(a) => turn::act(d, conn, &a.thread_id, &a.item_id, &a.action).await,
            Err(e) => Err(e),
        },
        "agent.preview" => args::<Preview>(p)
            .and_then(|p| turn::preview(d, p.thread_id.as_deref(), p.text.as_deref())),
        _ => return None,
    })
}

impl Agents {
    pub(crate) fn allow_send(&self, thread: &str) -> bool {
        let mut m = lock(&self.sends);
        let q = m.entry(thread.to_owned()).or_default();
        let now = Instant::now();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            q.pop_front();
        }
        if q.len() >= SENDS_PER_MINUTE {
            return false;
        }
        q.push_back(now);
        true
    }
}
