//! Dispatch: from an operator reference to the code that runs it.
//!
//! 1. `name@major` is parsed and looked up in the built-in catalogue (O-03 section 7) and the manifests.
//! 2. `source` operators (`stream.*`) are `op.not_runnable`: the ingest bridge runs them (O-10).
//! 3. Run-context placeholders in `config` are expanded and its `secret_ref`s resolved.
//! 4. `core` operators run in process under a deadline of `timeout_ms`; adapter, code and model operators
//!    run as sandboxed children through [`crate::runner`] after their `config_schema` check.
//! 5. Model operators follow O-03 section 8.2: refused with `llm.not_configured` without model access or
//!    a ladder (`<state_dir>/llm.json`); `config.ladder` is set to the ladder (narrowed to the node's
//!    `config.providers`); the enabled providers' `api_key_secret`s go into the start line's `secrets`; the
//!    attempt's reserved budget is the `config.llm_budget_usd_micro` the daemon put in the invocation.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ogun_ir::catalogue::{Catalogue, OpKind};
use ogun_types::{ErrorClass, OgunError};
use serde_json::{Map, Value};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::context::RunContext;
use crate::error::{OpError, OpResult, config};
use crate::manifest::{Determinism, Installed, LoadedManifest, ManifestSource, discover};
use crate::ops::{
    CoreFail, Job, archive, blocking, files_ops, parse, records, rollup, session, simple,
};
use crate::secrets::ResolvedSecrets;
use crate::{Context, Environment, Invocation, Outcome, Registry, files, runner};

/// What an operator reference resolves to, for the run record and lineage (O-03 section 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOp {
    /// Dotted name.
    pub name: String,
    /// Major version.
    pub major: u64,
    /// Exact version that runs (`ogun-ops`'s own version for core operators).
    pub version: String,
    /// Kind.
    pub kind: OpKind,
    /// BLAKE3 of the manifest's JCS form; `None` for core and source operators, which have no manifest.
    pub manifest_blake3: Option<String>,
    /// `true` when an installed manifest replaces the built-in one.
    pub installed: bool,
    /// The determinism class, when the manifest declares one.
    pub determinism: Option<Determinism>,
}

/// The registry's loaded state.
#[derive(Debug, Default)]
pub struct Inner {
    builtin: BTreeMap<(String, u64), Arc<LoadedManifest>>,
    /// Installed manifests and refused files.
    pub installed: Installed,
    acquire: crate::acquire::AcquireEngine,
}

impl Inner {
    /// Loads the built-in manifests and discovers the installed ones.
    #[must_use]
    pub fn load(env: &Environment) -> Self {
        let mut builtin = BTreeMap::new();
        for doc in crate::builtin::documents() {
            match LoadedManifest::from_value(&doc, ManifestSource::Builtin) {
                Ok(m) => {
                    builtin.insert((m.manifest.name.clone(), m.manifest.major()), Arc::new(m));
                }
                Err(e) => tracing::error!(error = %e, "a built-in operator manifest is invalid"),
            }
        }
        let installed = discover(&env.operator_dirs);
        for (key, m) in &installed.manifests {
            if builtin.contains_key(key) {
                tracing::info!(op = %key.0, manifest = ?m.source, "an installed manifest replaces the built-in one");
            }
        }
        for (file, reason) in &installed.refused {
            tracing::warn!(file = %file.display(), reason = %reason, "refused an operator manifest");
        }
        Self {
            builtin,
            installed,
            acquire: crate::acquire::AcquireEngine::default(),
        }
    }

    fn manifest(&self, name: &str, major: u64) -> Option<&Arc<LoadedManifest>> {
        let key = (name.to_owned(), major);
        self.installed
            .manifests
            .get(&key)
            .or_else(|| self.builtin.get(&key))
    }
}

/// `llm.not_configured` (registered by the model operators' crate, O-03 section 8.10).
pub(crate) fn llm_not_configured(message: &str) -> OpError {
    llm_error("llm.not_configured", ErrorClass::Unavailable, message)
}

fn llm_error(code: &str, class: ErrorClass, message: &str) -> OpError {
    match ogun_types::code_info(code) {
        Some(_) => OpError::new(code, message),
        // Built literally until the code table that registers the `llm` namespace is merged.
        None => OpError::from(OgunError {
            code: code.to_owned(),
            class,
            message: message.to_owned(),
            retryable: false,
            details: None,
        }),
    }
}

enum Target<'a> {
    Core(&'a str),
    Source,
    Child(Arc<LoadedManifest>),
}

fn target<'a>(reg: &Registry, op: &'a str) -> OpResult<(Target<'a>, u64)> {
    let unknown = || {
        OpError::new(
            "op.unknown",
            "no operator with this name and major version is installed",
        )
    };
    let r = ogun_ir::parse_op_ref(op).ok_or_else(unknown)?;
    let catalogue = Catalogue::builtin();
    let spec = catalogue
        .get(r.name)
        .filter(|s| s.majors.contains(&r.major));
    if let Some(m) = reg.inner.manifest(r.name, r.major)
        && spec.is_none_or(|s| matches!(s.kind, OpKind::Adapter | OpKind::Code | OpKind::Model))
    {
        return Ok((Target::Child(m.clone()), r.major));
    }
    match spec.map(|s| s.kind) {
        Some(OpKind::Core) => Ok((Target::Core(r.name), r.major)),
        Some(OpKind::Source) => Ok((Target::Source, r.major)),
        Some(_) => {
            let refused = reg.inner.installed.refused.iter().any(|(p, _)| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(r.name))
            });
            Err(OpError::new(
                "op.manifest_invalid",
                if refused {
                    "the operator's installed manifest was refused (see the daemon log)"
                } else {
                    "the operator has no manifest"
                },
            ))
        }
        None => Err(unknown()),
    }
}

/// `Registry::knows`.
pub(crate) fn knows(reg: &Registry, op: &str) -> bool {
    match target(reg, op) {
        Ok((Target::Core(_), _)) => true,
        Ok((Target::Source, _)) | Err(_) => false,
        Ok((Target::Child(m), _)) => {
            m.manifest.kind != crate::manifest::ManifestKind::Model || reg.env.model_access
        }
    }
}

/// `Registry::resolve`.
pub(crate) fn resolve(reg: &Registry, op: &str) -> Result<ResolvedOp, OgunError> {
    let (t, major) = target(reg, op).map_err(|e| e.error)?;
    let name = op.split('@').next().unwrap_or(op).to_owned();
    Ok(match t {
        Target::Core(_) => ResolvedOp {
            name,
            major,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            kind: OpKind::Core,
            manifest_blake3: None,
            installed: false,
            determinism: None,
        },
        Target::Source => ResolvedOp {
            name,
            major,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            kind: OpKind::Source,
            manifest_blake3: None,
            installed: false,
            determinism: Some(Determinism::Nondeterministic),
        },
        Target::Child(m) => ResolvedOp {
            name,
            major,
            version: m.manifest.version.clone(),
            kind: m.manifest.kind.op_kind(),
            manifest_blake3: Some(m.blake3.clone()),
            installed: matches!(m.source, ManifestSource::Installed(_)),
            determinism: Some(m.manifest.determinism),
        },
    })
}

/// `Registry::run`.
pub(crate) async fn run(reg: &Registry, inv: Invocation, ctx: Context) -> Outcome {
    if *ctx.cancel.borrow() {
        return Outcome::Cancelled;
    }
    match run_inner(reg, &inv, &ctx).await {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!(op = %inv.op, node_instance = %inv.node_instance, code = %e.error.code, "operator attempt failed");
            e.into_outcome()
        }
    }
}

async fn run_inner(reg: &Registry, inv: &Invocation, ctx: &Context) -> OpResult<Outcome> {
    let (t, _) = target(reg, &inv.op)?;
    if matches!(t, Target::Source) {
        return Err(OpError::new(
            "op.not_runnable",
            "stream sources run in the ogun-ingest bridge, not in ogund",
        ));
    }
    files::ensure_dir(&inv.scratch)?;
    let run_ctx = RunContext::of(inv);
    let raw = if inv.config.is_null() {
        Value::Object(Map::new())
    } else {
        inv.config.clone()
    };
    if !raw.is_object() {
        return Err(config("the node configuration is not an object"));
    }
    let expanded = run_ctx.expand(&raw)?;
    let mut secrets = ResolvedSecrets::resolve(&expanded, ctx.secrets.as_ref())?;
    match t {
        Target::Source => Err(OpError::new(
            "op.not_runnable",
            "stream sources run in the ingest bridge",
        )),
        Target::Core(name) => run_core(reg, name, inv, ctx, run_ctx, expanded, &secrets).await,
        Target::Child(manifest) => {
            manifest.check_config(&expanded)?;
            let mut config = expanded;
            if manifest.manifest.kind == crate::manifest::ManifestKind::Model {
                prepare_model(&reg.env, &mut config, &mut secrets, ctx)?;
            }
            Ok(runner::run(runner::ChildRun {
                env: &reg.env,
                manifest: &manifest,
                inv,
                config: &config,
                secrets: &secrets,
                ctx,
            })
            .await)
        }
    }
}

/// O-03 section 8.2 items 1 to 4.
fn prepare_model(
    env: &Environment,
    config: &mut Value,
    secrets: &mut ResolvedSecrets,
    ctx: &Context,
) -> OpResult<()> {
    if !env.model_access {
        return Err(llm_not_configured(
            "model access is off for this environment",
        ));
    }
    let path = env.state_dir.join("llm.json");
    let text = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 1 << 20 => String::from_utf8(bytes).map_err(|_| {
            llm_error(
                "llm.config_invalid",
                ErrorClass::InvalidRequest,
                "llm.json is not UTF-8",
            )
        })?,
        Ok(_) => {
            return Err(llm_error(
                "llm.config_invalid",
                ErrorClass::InvalidRequest,
                "llm.json is larger than 1 MiB",
            ));
        }
        Err(_) => {
            return Err(llm_not_configured(
                "no model ladder is configured (llm.json)",
            ));
        }
    };
    zeus_verify::canonical::reject_duplicate_keys(&text).map_err(|_| {
        llm_error(
            "llm.config_invalid",
            ErrorClass::InvalidRequest,
            "llm.json is not JSON or has duplicate members",
        )
    })?;
    let mut ladder: Value = serde_json::from_str(&text).map_err(|_| {
        llm_error(
            "llm.config_invalid",
            ErrorClass::InvalidRequest,
            "llm.json is not JSON",
        )
    })?;
    let only: Option<Vec<String>> = match config.get("providers") {
        None | Some(Value::Null) => None,
        Some(Value::Array(names)) => Some(
            names
                .iter()
                .map(|n| n.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    llm_error(
                        "llm.config_invalid",
                        ErrorClass::InvalidRequest,
                        "providers must be ladder names",
                    )
                })?,
        ),
        Some(_) => {
            return Err(llm_error(
                "llm.config_invalid",
                ErrorClass::InvalidRequest,
                "providers must be a list",
            ));
        }
    };
    let providers = ladder
        .get_mut("providers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            llm_error(
                "llm.config_invalid",
                ErrorClass::InvalidRequest,
                "llm.json has no providers list",
            )
        })?;
    if let Some(names) = &only {
        let known: Vec<&str> = providers
            .iter()
            .filter_map(|p| p.get("name").and_then(Value::as_str))
            .collect();
        if names.iter().any(|n| !known.contains(&n.as_str())) {
            return Err(llm_error(
                "llm.config_invalid",
                ErrorClass::InvalidRequest,
                "providers names a provider the ladder does not have",
            ));
        }
        providers.retain(|p| {
            p.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| names.iter().any(|x| x == n))
        });
    }
    let enabled = |p: &Value| p.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    if !providers.iter().any(enabled) {
        return Err(llm_not_configured(
            "the model ladder has no enabled provider for this node",
        ));
    }
    for p in providers.iter().filter(|p| enabled(p)) {
        if let Some(name) = p.get("api_key_secret").and_then(Value::as_str) {
            // A key that does not resolve is left out; the operator skips that provider.
            let _ = secrets.add_optional(name, ctx.secrets.as_ref());
        }
    }
    let budget = config
        .get("llm_budget_usd_micro")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            OpError::new(
                "run.budget_exhausted",
                "no model budget was reserved for this attempt (config.llm_budget_usd_micro)",
            )
        })?;
    let map = config.as_object_mut().ok_or_else(config_err)?;
    map.insert("ladder".into(), ladder);
    map.insert("llm_budget_usd_micro".into(), Value::from(budget));
    Ok(())
}

fn config_err() -> OpError {
    config("the node configuration is not an object")
}

/// A cancellation receiver that also fires at `deadline`, for blocking core operators.
fn cancel_with_deadline(
    upstream: &watch::Receiver<bool>,
    deadline: Instant,
) -> (watch::Receiver<bool>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = watch::channel(false);
    let mut up = upstream.clone();
    let task = tokio::spawn(async move {
        let mut live = true;
        loop {
            tokio::select! {
                r = up.changed(), if live => match r {
                    Ok(()) if *up.borrow() => {
                        let _ = tx.send(true);
                        return;
                    }
                    Ok(()) => {}
                    Err(_) => live = false,
                },
                () = tokio::time::sleep_until(deadline) => {
                    let _ = tx.send(true);
                    return;
                }
            }
        }
    });
    (rx, task)
}

async fn run_core(
    reg: &Registry,
    name: &str,
    inv: &Invocation,
    ctx: &Context,
    run_ctx: RunContext,
    config: Value,
    secrets: &ResolvedSecrets,
) -> OpResult<Outcome> {
    let timeout = Duration::from_millis(inv.timeout_ms.max(1000));
    let deadline = Instant::now() + timeout;
    let (cancel, watcher) = cancel_with_deadline(&ctx.cancel, deadline);
    let job = Job {
        op: name.to_owned(),
        inputs: inv.inputs.clone(),
        scratch: inv.scratch.clone(),
        config,
        run: run_ctx,
        workspace: reg.env.workspace.clone(),
        events: ctx.events.clone(),
        cancel,
    };
    let env = &reg.env;
    let result = match name {
        "hash" => blocking(job, simple::hash).await,
        "dedupe" => blocking(job, simple::dedupe).await,
        "manifest" => blocking(job, simple::manifest).await,
        "map" => blocking(job, simple::map).await,
        "join_metadata" => blocking(job, simple::join_metadata).await,
        "parse" => blocking(job, parse::parse).await,
        "export" => {
            let fut = crate::export::export(&job, env);
            tokio::time::timeout(timeout, fut)
                .await
                .unwrap_or(Err(CoreFail::Cancelled))
        }
        "filter" => blocking(job, records::filter).await,
        "partition" => blocking(job, records::partition).await,
        "file.copy" => blocking(job, files_ops::copy).await,
        "file.move" => blocking(job, files_ops::mv).await,
        "archive.extract" => blocking(job, archive::extract).await,
        "window.rollup" => blocking(job, rollup::rollup).await,
        "crawl" => {
            let fut = crate::crawl::crawl(&job, env, secrets);
            tokio::time::timeout(timeout, fut)
                .await
                .unwrap_or(Err(CoreFail::Cancelled))
        }
        "acquire" => {
            // The engine pauses its jobs a little before the deadline so the next attempt resumes them.
            let inner_deadline = deadline - Duration::from_millis(500).min(timeout / 4);
            let node_instance = inv.node_instance.to_string();
            let fut = crate::acquire::acquire(
                &job,
                env,
                secrets,
                &reg.inner.acquire,
                &node_instance,
                inv.attempt,
                inner_deadline,
            );
            tokio::time::timeout(timeout, fut)
                .await
                .unwrap_or(Err(CoreFail::Cancelled))
        }
        "zeus.session_acquire" => {
            watcher.abort();
            return Ok(session::run(inv, &job));
        }
        _ => Err(CoreFail::Op(OpError::new(
            "op.unknown",
            "this build has no such core operator",
        ))),
    };
    watcher.abort();
    let upstream_cancelled = *ctx.cancel.borrow();
    match result {
        Ok(outputs) => Ok(Outcome::Succeeded { outputs }),
        Err(CoreFail::Op(e)) => Err(e),
        Err(CoreFail::Cancelled) if upstream_cancelled => Ok(Outcome::Cancelled),
        Err(CoreFail::Cancelled) if Instant::now() >= deadline - Duration::from_millis(600) => {
            Err(OpError::new(
                "op.timeout",
                format!(
                    "{name} exceeded the attempt's timeout of {} ms",
                    inv.timeout_ms
                ),
            ))
        }
        Err(CoreFail::Cancelled) => Ok(Outcome::Cancelled),
    }
}
