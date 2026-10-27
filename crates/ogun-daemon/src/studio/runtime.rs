//! Run defaults, per-workflow overrides and the scheduler switch (O-16 section 5).
//!
//! Which members the daemon applies today, and which it only stores, is reported with every read (`enforced`), so
//! Studio never shows a setting as active that nothing reads.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use ogun_api::{ApiError, ApiResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Docs, args, out};
use crate::server::Conn;
use crate::state::{Daemon, lock};

/// Members the daemon applies today.
pub const ENFORCED: &[&str] = &[
    "concurrency",
    "approval_expiry_hours",
    "model_access",
    "auto_pause_after_failures",
];

/// Run defaults (the shape Studio shows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    /// Attempts running at once.
    pub concurrency: u32,
    /// Default retries per node.
    pub retries: u32,
    /// Default wall-clock budget, minutes; 0 means none.
    pub wall_minutes: u64,
    /// Default bytes budget, GiB; 0 means none.
    pub bytes_gib: u64,
    /// How long an approval waits before it expires.
    pub approval_expiry_hours: u64,
    /// `off` or `ladder`.
    pub model_access: String,
    /// Pause a schedule after this many failed runs in a row; 0 means never.
    pub auto_pause_after_failures: u32,
    /// Archive finished runs after this many days; 0 means never.
    pub auto_archive_days: u32,
    /// Never auto-archive a run that needs attention.
    pub keep_attention_runs: bool,
    /// Start runs that were in flight when the daemon last stopped.
    pub resume_on_start: bool,
}

impl RuntimeSettings {
    fn defaults(parallel: u32) -> Self {
        Self {
            concurrency: parallel.max(1),
            retries: 2,
            wall_minutes: 0,
            bytes_gib: 0,
            approval_expiry_hours: 168,
            model_access: "ladder".into(),
            auto_pause_after_failures: 0,
            auto_archive_days: 0,
            keep_attention_runs: true,
            resume_on_start: true,
        }
    }

    fn check(&self) -> ApiResult<()> {
        let bad = |what: &str| Err(ApiError::invalid_params(format!("runtime: {what}")));
        if !(1..=64).contains(&self.concurrency) {
            return bad("concurrency is 1 to 64");
        }
        if self.retries > 20 {
            return bad("retries is 0 to 20");
        }
        if self.wall_minutes > 100_000 || self.bytes_gib > 100_000 {
            return bad("wall_minutes and bytes_gib are at most 100000");
        }
        if !(1..=8760).contains(&self.approval_expiry_hours) {
            return bad("approval_expiry_hours is 1 to 8760");
        }
        if !matches!(self.model_access.as_str(), "off" | "ladder") {
            return bad("model_access is off or ladder");
        }
        if self.auto_pause_after_failures > 1000 || self.auto_archive_days > 3650 {
            return bad(
                "auto_pause_after_failures is at most 1000 and auto_archive_days at most 3650",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RuntimeFile {
    defaults: Option<RuntimeSettings>,
    #[serde(default)]
    overrides: BTreeMap<String, BTreeMap<String, Value>>,
    scheduler_enabled: Option<bool>,
}

/// The loaded run defaults.
#[derive(Debug)]
pub struct RuntimeState {
    file: Mutex<RuntimeFile>,
    scheduler_on: AtomicBool,
    parallel_default: u32,
}

impl RuntimeState {
    /// Loads `settings/runtime.json`.
    #[must_use]
    pub fn load(docs: &Docs, scheduler_default: bool, parallel_default: u32) -> Self {
        let file: RuntimeFile = docs.read("runtime");
        let on = file.scheduler_enabled.unwrap_or(scheduler_default);
        Self {
            file: Mutex::new(file),
            scheduler_on: AtomicBool::new(on),
            parallel_default,
        }
    }

    fn defaults(&self) -> RuntimeSettings {
        lock(&self.file)
            .defaults
            .clone()
            .filter(|d| d.check().is_ok())
            .unwrap_or_else(|| RuntimeSettings::defaults(self.parallel_default))
    }

    /// The settings that apply to a workflow: its overrides over the defaults.
    #[must_use]
    pub fn effective(&self, workflow: Option<&str>) -> RuntimeSettings {
        let mut v = serde_json::to_value(self.defaults()).unwrap_or(Value::Null);
        if let (Some(w), Some(obj)) = (workflow, v.as_object_mut())
            && let Some(over) = lock(&self.file).overrides.get(w)
        {
            for (k, x) in over {
                obj.insert(k.clone(), x.clone());
            }
        }
        serde_json::from_value::<RuntimeSettings>(v)
            .ok()
            .filter(|s| s.check().is_ok())
            .unwrap_or_else(|| self.defaults())
    }

    /// Attempts allowed at once.
    #[must_use]
    pub fn concurrency(&self) -> usize {
        self.defaults().concurrency as usize
    }

    /// How long an approval waits.
    #[must_use]
    pub fn approval_ttl_ms(&self) -> u64 {
        self.defaults()
            .approval_expiry_hours
            .saturating_mul(3_600_000)
    }

    /// Whether model operators may run.
    #[must_use]
    pub fn model_access(&self) -> bool {
        self.defaults().model_access == "ladder"
    }

    /// Failed runs in a row after which a schedule pauses (0 means never), for a workflow.
    #[must_use]
    pub fn auto_pause_after(&self, workflow: &str) -> u32 {
        self.effective(Some(workflow)).auto_pause_after_failures
    }

    /// Whether triggers fire.
    #[must_use]
    pub fn scheduler_enabled(&self) -> bool {
        self.scheduler_on.load(Ordering::Relaxed)
    }
}

fn view(state: &RuntimeState, workflow: Option<&str>) -> Value {
    let overridden: Vec<String> = workflow
        .and_then(|w| {
            lock(&state.file)
                .overrides
                .get(w)
                .map(|o| o.keys().cloned().collect())
        })
        .unwrap_or_default();
    json!({
        "values": state.effective(workflow),
        "scope_workflow": workflow,
        "overridden": overridden,
        "enforced": ENFORCED,
    })
}

#[derive(Deserialize)]
struct GetParams {
    #[serde(default)]
    workflow: Option<String>,
}

#[derive(Deserialize)]
struct SetParams {
    #[serde(default)]
    workflow: Option<String>,
    patch: serde_json::Map<String, Value>,
    #[serde(default)]
    #[allow(dead_code)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
struct ResetParams {
    workflow: String,
    field: String,
    #[serde(default)]
    #[allow(dead_code)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
struct SchedulerParams {
    enabled: bool,
}

/// Routes `runtime.*` and `scheduler.*`.
pub fn dispatch(d: &Arc<Daemon>, conn: &Conn, method: &str, p: Value) -> Option<ApiResult<Value>> {
    let r = &d.studio.runtime;
    Some(match method {
        "runtime.get" => args::<GetParams>(p).and_then(|g| {
            if let Some(w) = &g.workflow {
                d.workflow_row(w)?;
            }
            Ok(view(r, g.workflow.as_deref()))
        }),
        "runtime.set" => args::<SetParams>(p).and_then(|s| {
            let mut file = lock(&r.file);
            match &s.workflow {
                None => {
                    let mut v = serde_json::to_value(
                        file.defaults
                            .clone()
                            .unwrap_or_else(|| RuntimeSettings::defaults(r.parallel_default)),
                    )
                    .map_err(|e| ApiError::internal(e.to_string()))?;
                    if let Some(obj) = v.as_object_mut() {
                        for (k, x) in s.patch {
                            obj.insert(k, x);
                        }
                    }
                    let next: RuntimeSettings = serde_json::from_value(v)
                        .map_err(|e| ApiError::invalid_params(format!("runtime: {e}")))?;
                    next.check()?;
                    file.defaults = Some(next);
                }
                Some(w) => {
                    d.workflow_row(w)?;
                    let mut v = serde_json::to_value(
                        file.defaults
                            .clone()
                            .unwrap_or_else(|| RuntimeSettings::defaults(r.parallel_default)),
                    )
                    .map_err(|e| ApiError::internal(e.to_string()))?;
                    let entry = file.overrides.entry(w.clone()).or_default();
                    if let Some(obj) = v.as_object_mut() {
                        for (k, x) in entry.iter() {
                            obj.insert(k.clone(), x.clone());
                        }
                        for (k, x) in &s.patch {
                            if !obj.contains_key(k) {
                                return Err(ApiError::invalid_params(format!(
                                    "runtime: no setting named {k}"
                                )));
                            }
                            obj.insert(k.clone(), x.clone());
                        }
                    }
                    serde_json::from_value::<RuntimeSettings>(v)
                        .map_err(|e| ApiError::invalid_params(format!("runtime: {e}")))?
                        .check()?;
                    for (k, x) in s.patch {
                        entry.insert(k, x);
                    }
                }
            }
            let snapshot = file.clone();
            drop(file);
            d.studio.docs.write("runtime", &snapshot)?;
            tracing::info!(by = %conn.label(), workflow = ?s.workflow, "run defaults changed");
            Ok(view(r, s.workflow.as_deref()))
        }),
        "runtime.reset" => args::<ResetParams>(p).and_then(|s| {
            let mut file = lock(&r.file);
            if let Some(o) = file.overrides.get_mut(&s.workflow) {
                o.remove(&s.field);
                if o.is_empty() {
                    file.overrides.remove(&s.workflow);
                }
            }
            let snapshot = file.clone();
            drop(file);
            d.studio.docs.write("runtime", &snapshot)?;
            Ok(view(r, Some(&s.workflow)))
        }),
        "scheduler.set_enabled" => args::<SchedulerParams>(p).and_then(|s| {
            r.scheduler_on.store(s.enabled, Ordering::Relaxed);
            let mut file = lock(&r.file);
            file.scheduler_enabled = Some(s.enabled);
            let snapshot = file.clone();
            drop(file);
            d.studio.docs.write("runtime", &snapshot)?;
            tracing::info!(by = %conn.label(), enabled = s.enabled, "scheduler switch changed");
            out(&json!({ "scheduler_enabled": s.enabled }))
        }),
        _ => return None,
    })
}
