//! Starts, restarts and stops sources as workflows change in `ogund` (O-10 section 2.1).
//!
//! Every `refresh_ms` the supervisor re-reads `ingest.json` and the workflow list. A new stream
//! source gets a log, an adapter and a forwarder. A source whose workflow was disabled or removed
//! stops acknowledging at once (its adapter is aborted) while its forwarder drains the log into
//! `ogund`. A changed node configuration or `ingest.json` entry restarts the adapter only; a
//! changed buffer or window drains and reopens the log. When `ogund` is unreachable nothing is
//! stopped: the sources keep logging, which is the point of store-and-forward. The last sources
//! seen are cached in `<log_dir>/sources.json`, so a bridge that starts while `ogund` is down
//! still ingests.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ogun_api::{Client, OgunPaths};
use tokio::sync::watch;

use crate::applog::{BatchPolicy, LogHandle};
use crate::config::{IngestConfig, SourceOverrides};
use crate::decode::{Shaper, now_ms};
use crate::forward::{self, Backoff};
use crate::sources::{self, Ctx};
use crate::spec::{SourceSpec, discover};
use crate::status::Counters;

struct Running {
    spec: Arc<SourceSpec>,
    overrides: SourceOverrides,
    log: LogHandle,
    log_thread: Option<std::thread::JoinHandle<()>>,
    counters: Arc<Counters>,
    adapter: tokio::task::JoinHandle<()>,
    forwarder: tokio::task::JoinHandle<()>,
    drain: watch::Sender<bool>,
}

/// The bridge's supervisor.
pub struct Supervisor {
    paths: OgunPaths,
    config_path: PathBuf,
    credential: String,
    config: IngestConfig,
    running: BTreeMap<String, Running>,
}

impl Supervisor {
    /// A supervisor for one state directory.
    pub fn new(paths: OgunPaths, config_path: PathBuf, credential: String) -> Self {
        Self {
            paths,
            config_path,
            credential,
            config: IngestConfig::default(),
            running: BTreeMap::new(),
        }
    }

    fn log_dir(&self) -> PathBuf {
        self.config
            .log_dir
            .clone()
            .unwrap_or_else(|| self.paths.state_dir.join("ingest"))
    }

    fn reload_config(&mut self) {
        match IngestConfig::load(&self.config_path) {
            Ok(c) => self.config = c,
            Err(e) => {
                tracing::error!(error = %e, "ingest.json is invalid; keeping the previous settings")
            }
        }
    }

    fn cache_file(&self) -> PathBuf {
        self.log_dir().join("sources.json")
    }

    fn save_cache(&self, specs: &[SourceSpec]) {
        let dir = self.log_dir();
        let result = std::fs::create_dir_all(&dir).and_then(|()| {
            let tmp = dir.join("sources.json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(specs).unwrap_or_default())?;
            std::fs::rename(&tmp, self.cache_file())
        });
        if let Err(e) = result {
            tracing::warn!(error = ?e.kind(), "cannot cache the source list");
        }
    }

    fn load_cache(&self) -> Option<Vec<SourceSpec>> {
        serde_json::from_slice(&std::fs::read(self.cache_file()).ok()?).ok()
    }

    /// Runs until the task is dropped.
    pub async fn run(&mut self) {
        let mut control: Option<Client> = None;
        let mut backoff = Backoff::default();
        let mut first = true;
        self.reload_config();
        let mut status = tokio::time::interval(Duration::from_millis(
            self.config.status_interval_ms.max(1_000),
        ));
        status.tick().await;
        loop {
            self.reload_config();
            if control.is_none() {
                match forward::connect(&self.paths, &self.credential).await {
                    Ok(c) => {
                        backoff.reset();
                        control = Some(c);
                    }
                    Err(e) => {
                        tracing::warn!(code = e.code(), "cannot reach ogund; sources keep logging");
                    }
                }
            }
            let mut found = None;
            if let Some(c) = control.as_mut() {
                match discover(c, &self.config.workflows).await {
                    Ok(specs) => found = Some(specs),
                    Err(e) => {
                        tracing::warn!(code = e.code(), "cannot list workflows");
                        if e.code() == "api.unavailable" {
                            control = None;
                        }
                    }
                }
            }
            match found {
                Some(specs) => {
                    self.save_cache(&specs);
                    self.reconcile(specs).await;
                }
                None if first => {
                    if let Some(cached) = self.load_cache() {
                        tracing::info!(
                            sources = cached.len(),
                            "starting the cached sources while ogund is unreachable"
                        );
                        self.reconcile(cached).await;
                    }
                }
                None => self.restart_dead_adapters(),
            }
            first = false;
            let wait = if control.is_some() {
                Duration::from_millis(self.config.refresh_ms.max(1_000))
            } else {
                backoff.next()
            };
            let deadline = tokio::time::Instant::now() + wait;
            loop {
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => break,
                    _ = status.tick() => self.log_status().await,
                }
            }
        }
    }

    fn ctx(
        &self,
        spec: &Arc<SourceSpec>,
        overrides: &SourceOverrides,
        log: &LogHandle,
        counters: &Arc<Counters>,
    ) -> Ctx {
        Ctx {
            spec: spec.clone(),
            overrides: overrides.clone(),
            secrets: self.config.secrets.clone(),
            log: log.clone(),
            counters: counters.clone(),
            shaper: Shaper::new(spec.window.clone()),
            state_dir: self.log_dir().join(&spec.workflow),
        }
    }

    fn spawn_adapter(ctx: Ctx) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let id = ctx.spec.id();
            let counters = ctx.counters.clone();
            tracing::info!(source = %id, op = ctx.spec.op.as_str(), "source starting");
            if let Err(e) = sources::run(ctx).await {
                counters.set_error(e.clone());
                tracing::error!(source = %id, error = %e, "source stopped; retrying at the next refresh");
            }
        })
    }

    async fn start(&mut self, spec: SourceSpec) {
        let id = spec.id();
        let overrides = self.config.overrides(&spec.workflow, &spec.node);
        let counters = Arc::new(Counters::default());
        let path = self
            .log_dir()
            .join(&spec.workflow)
            .join(format!("{}.redb", spec.node));
        let buffer = spec.buffer.clone();
        let policy = BatchPolicy {
            max_records: self.config.batch_max_records.max(1),
            max_delay: Duration::from_millis(self.config.batch_max_delay_ms),
        };
        let c = counters.clone();
        let opened =
            tokio::task::spawn_blocking(move || LogHandle::open(&path, &buffer, policy, c)).await;
        let (log, thread) = match opened {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::error!(source = %id, error = %e, "cannot open the ingest log");
                return;
            }
            Err(_) => return,
        };
        let spec = Arc::new(spec);
        let ctx = self.ctx(&spec, &overrides, &log, &counters);
        let adapter = Self::spawn_adapter(ctx);
        let (drain, drain_rx) = watch::channel(false);
        let forwarder = tokio::spawn(forward::run(
            spec.clone(),
            log.clone(),
            counters.clone(),
            self.paths.clone(),
            self.credential.clone(),
            drain_rx,
        ));
        self.running.insert(
            id,
            Running {
                spec,
                overrides,
                log,
                log_thread: Some(thread),
                counters,
                adapter,
                forwarder,
                drain,
            },
        );
    }

    async fn stop(&mut self, id: &str) {
        let Some(mut r) = self.running.remove(id) else {
            return;
        };
        r.adapter.abort();
        let _ = r.drain.send(true);
        tracing::info!(source = %id, "source stopped; draining its log into ogund");
        if tokio::time::timeout(Duration::from_secs(120), &mut r.forwarder)
            .await
            .is_err()
        {
            tracing::warn!(source = %id, "drain did not finish; records stay in the bridge log");
            r.forwarder.abort();
        }
        r.log.close();
        if let Some(t) = r.log_thread.take() {
            let _ = tokio::task::spawn_blocking(move || t.join()).await;
        }
    }

    async fn reconcile(&mut self, specs: Vec<SourceSpec>) {
        let desired: BTreeMap<String, SourceSpec> =
            specs.into_iter().map(|s| (s.id(), s)).collect();
        let gone: Vec<String> = self
            .running
            .keys()
            .filter(|id| !desired.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            self.stop(&id).await;
        }
        for (id, spec) in desired {
            let overrides = self.config.overrides(&spec.workflow, &spec.node);
            let Some(r) = self.running.get(&id) else {
                self.start(spec).await;
                continue;
            };
            if r.spec.buffer != spec.buffer || r.spec.window != spec.window {
                tracing::info!(source = %id, "buffer or window settings changed; reopening the log");
                self.stop(&id).await;
                self.start(spec).await;
            } else if *r.spec != spec || r.overrides != overrides || r.adapter.is_finished() {
                let spec = Arc::new(spec);
                let ctx = self.ctx(&spec, &overrides, &r.log, &r.counters);
                if let Some(r) = self.running.get_mut(&id) {
                    r.adapter.abort();
                    r.spec = spec;
                    r.overrides = overrides;
                    r.adapter = Self::spawn_adapter(ctx);
                }
            }
        }
    }

    fn restart_dead_adapters(&mut self) {
        let dead: Vec<String> = self
            .running
            .iter()
            .filter(|(_, r)| r.adapter.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in dead {
            let Some(r) = self.running.get(&id) else {
                continue;
            };
            let ctx = self.ctx(&r.spec, &r.overrides, &r.log, &r.counters);
            if let Some(r) = self.running.get_mut(&id) {
                r.adapter = Self::spawn_adapter(ctx);
            }
        }
    }

    async fn log_status(&self) {
        for (id, r) in &self.running {
            let c = &r.counters;
            let (stats, len) = match r.log.stats().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let lag_ms = match r.log.read(0, 1).await {
                Ok(v) => v
                    .first()
                    .map_or(0, |l| now_ms().saturating_sub(l.record.received_ms)),
                Err(_) => 0,
            };
            tracing::info!(
                source = %id,
                messages = Counters::get(&c.messages),
                written = Counters::get(&c.written),
                invalid = Counters::get(&c.invalid),
                dropped_newest = Counters::get(&c.dropped_newest),
                refused = Counters::get(&c.refused),
                forwarded = Counters::get(&c.forwarded),
                lag_records = len,
                lag_ms,
                bytes_used = stats.bytes_used,
                lost_oldest = stats.lost_oldest,
                evicted_released = stats.evicted_released,
                daemon_dropped = Counters::get(&c.daemon_dropped),
                daemon_late = Counters::get(&c.daemon_late),
                push_errors = Counters::get(&c.push_errors),
                source_errors = Counters::get(&c.source_errors),
                gaps = Counters::get(&c.gaps),
                last_error = c.last_error().as_deref().unwrap_or(""),
                "ingest status"
            );
        }
    }

    /// Stops every adapter, commits what is pending and closes the logs. Records stay in the
    /// bridge's logs and are forwarded at the next start.
    pub async fn shutdown(&mut self) {
        let ids: Vec<String> = self.running.keys().cloned().collect();
        for id in ids {
            if let Some(mut r) = self.running.remove(&id) {
                r.adapter.abort();
                r.forwarder.abort();
                r.log.close();
                if let Some(t) = r.log_thread.take() {
                    let _ = tokio::task::spawn_blocking(move || t.join()).await;
                }
            }
        }
    }
}
