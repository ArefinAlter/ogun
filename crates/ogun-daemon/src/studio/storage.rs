//! Storage: what the artifact store holds, the rules for what may be removed, and a cleanup that says what it
//! would remove before it removes it (O-16 section 6).
//!
//! Rules are days; `0` is off, and every removal rule is off by default. Cleanup removes the artifact records of
//! the runs a rule selects (the artifact store tombstones them and then collects the content nothing else
//! references), and clears the engine's caches. A run's record, its events and its lineage stay in the run
//! store: a partition that finished stays finished, and the run page keeps saying what ran, but the removed
//! bytes are gone and a later run of that partition makes them again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ogun_api::{ApiError, ApiResult};
use ogun_run::{NodeState, RunState, derive_run_state};
use ogun_store::{Collection, NodeInstance, RunRecord};
use ogun_types::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Docs, args, out};
use crate::error::store_err;
use crate::server::Conn;
use crate::state::{Daemon, OutputRecord};

const GIB: u64 = 1 << 30;
const DAY_MS: i64 = 86_400_000;

/// The removal and retention rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StorageRules {
    /// Where the artifact store is (shown; it cannot be moved from here).
    pub store_location: String,
    /// Artifact store size limit in GiB; 0 is no limit. Applied when the engine starts.
    pub limit_gib: u64,
    /// Remove the artifacts of runs older than this many days.
    pub retention_days: u32,
    /// Remove the artifacts of failed runs after this many days.
    pub delete_failed_after_days: u32,
    /// Remove the artifacts of cancelled runs after this many days.
    pub delete_cancelled_after_days: u32,
    /// Remove partial outputs (failed or cancelled node instances) after this many days.
    pub delete_partials_after_days: u32,
    /// Not used: the engine keeps no log files apart from its run events.
    pub delete_logs_after_days: u32,
    /// Clear the engine's scratch and download caches after this many days.
    pub clear_caches_after_days: u32,
    /// Never remove the newest successful output of a partition.
    pub keep_lineage_referenced: bool,
}

impl Default for StorageRules {
    fn default() -> Self {
        Self {
            store_location: String::new(),
            limit_gib: 0,
            retention_days: 0,
            delete_failed_after_days: 0,
            delete_cancelled_after_days: 0,
            delete_partials_after_days: 0,
            delete_logs_after_days: 0,
            clear_caches_after_days: 0,
            keep_lineage_referenced: true,
        }
    }
}

fn rules_of(d: &Daemon) -> StorageRules {
    let mut r: StorageRules = d.studio.docs.read("storage");
    r.store_location = d.paths.artifact_dir().display().to_string();
    r
}

/// The artifact store quota the rules ask for, read before the daemon opens its stores.
#[must_use]
pub fn quota_from_rules(state_dir: &Path) -> Option<u64> {
    let bytes = std::fs::read(state_dir.join("settings").join("storage.json")).ok()?;
    let r: StorageRules = serde_json::from_slice(&bytes).ok()?;
    (r.limit_gib > 0).then(|| r.limit_gib.saturating_mul(GIB))
}

fn dir_bytes(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_bytes(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

fn older_than(p: &Path, days: u32) -> bool {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age >= Duration::from_secs(u64::from(days) * 86_400))
}

/// The cache directories: per-attempt scratch space and the engine's download area.
fn cache_dirs(d: &Daemon) -> Vec<PathBuf> {
    vec![
        d.paths.scratch_dir(),
        d.paths
            .state_dir
            .join("ops")
            .join("acquire")
            .join("downloads"),
    ]
}

/// Cache entries (immediate children) older than `days`, or all of them when `days` is 0.
fn cache_entries(d: &Daemon, days: u32) -> Vec<PathBuf> {
    cache_dirs(d)
        .into_iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|rd| rd.flatten().map(|e| e.path()))
        .filter(|p| days == 0 || older_than(p, days))
        .collect()
}

/// One output of one node instance, with where it came from.
struct Held {
    artifact_id: String,
    blake3: String,
    size: u64,
    workflow: String,
    run_age_days: i64,
    run_state: RunState,
    node_state: NodeState,
    head: bool,
    instance: String,
}

/// Days added to every run's age. Zero in any normal run; `OGUN_STORAGE_CLOCK_OFFSET_DAYS` exists so an acceptance
/// run can make data look old without waiting (the removal it then causes is real). Capped at ten years.
fn age_offset_ms() -> i64 {
    std::env::var("OGUN_STORAGE_CLOCK_OFFSET_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map_or(0, |days| days.clamp(0, 3650) * DAY_MS)
}

fn survey(d: &Daemon) -> ApiResult<Vec<Held>> {
    let now = Timestamp::now().unix_millis() + age_offset_ms();
    let mut runs: Vec<RunRecord> = Vec::new();
    let mut before = None;
    loop {
        let page = d.store.list_runs(before, 500).map_err(store_err)?;
        let more = page.len() == 500;
        before = page.last().map(|r| r.id);
        runs.extend(page);
        if !more {
            break;
        }
    }
    let mut held = Vec::new();
    // (workflow hash, node, partition) -> newest succeeded instance, whose output is the lineage head.
    let mut head: BTreeMap<(String, String, String), (i64, String)> = BTreeMap::new();
    let mut staged: Vec<(Held, String, String, String)> = Vec::new();
    for run in &runs {
        let instances: Vec<NodeInstance> = d.store.instances(run.id).map_err(store_err)?;
        let states: Vec<NodeState> = instances.iter().map(|i| i.state).collect();
        let run_state = derive_run_state(&states);
        let age = (now - run.created_at.unix_millis()) / DAY_MS;
        for i in instances {
            let outs: Vec<OutputRecord> = d
                .store
                .get_doc(Collection::Outputs, &i.id.to_string())
                .map_err(store_err)?
                .unwrap_or_default();
            for o in outs.into_iter().filter(|o| !o.removed) {
                let key = (
                    run.workflow_hash.clone(),
                    i.node.clone(),
                    i.partition.clone(),
                );
                if i.state == NodeState::Succeeded {
                    let at = o.committed_at.unix_millis();
                    let e = head.entry(key.clone()).or_insert((i64::MIN, String::new()));
                    if at > e.0 {
                        *e = (at, i.id.to_string());
                    }
                }
                staged.push((
                    Held {
                        artifact_id: o.artifact_id,
                        blake3: o.blake3,
                        size: o.size_bytes,
                        workflow: run.workflow.clone(),
                        run_age_days: age,
                        run_state,
                        node_state: i.state,
                        head: false,
                        instance: i.id.to_string(),
                    },
                    key.0,
                    key.1,
                    key.2,
                ));
            }
        }
    }
    for (mut h, hash, node, part) in staged {
        h.head = head
            .get(&(hash, node, part))
            .is_some_and(|(_, inst)| *inst == h.instance);
        held.push(h);
    }
    Ok(held)
}

#[derive(Default, Serialize)]
struct Line {
    rule: String,
    artifacts: u64,
    bytes: u64,
}

struct Plan {
    /// Artifact ids to remove, with the rule that chose them.
    remove: Vec<(String, String, u64)>,
    lines: Vec<Line>,
    caches: Vec<PathBuf>,
    caches_bytes: u64,
}

fn plan(d: &Daemon) -> ApiResult<Plan> {
    let rules = rules_of(d);
    let held = survey(d)?;
    let mut remove: Vec<(String, String, u64)> = Vec::new();
    let mut lines = Vec::new();
    type Pick = Box<dyn Fn(&Held) -> bool>;
    let finished = |s: RunState| {
        matches!(
            s,
            RunState::Succeeded | RunState::Failed | RunState::Cancelled
        )
    };
    let table: Vec<(String, Pick)> = vec![
        (
            format!(
                "Failed runs older than {} days",
                rules.delete_failed_after_days
            ),
            Box::new({
                let days = i64::from(rules.delete_failed_after_days);
                move |h| days > 0 && h.run_state == RunState::Failed && h.run_age_days >= days
            }),
        ),
        (
            format!(
                "Cancelled runs older than {} days",
                rules.delete_cancelled_after_days
            ),
            Box::new({
                let days = i64::from(rules.delete_cancelled_after_days);
                move |h| days > 0 && h.run_state == RunState::Cancelled && h.run_age_days >= days
            }),
        ),
        (
            format!(
                "Partial outputs older than {} days",
                rules.delete_partials_after_days
            ),
            Box::new({
                let days = i64::from(rules.delete_partials_after_days);
                move |h| {
                    days > 0
                        && matches!(h.node_state, NodeState::Failed | NodeState::Cancelled)
                        && h.run_age_days >= days
                }
            }),
        ),
        (
            format!("Finished runs older than {} days", rules.retention_days),
            Box::new({
                let days = i64::from(rules.retention_days);
                move |h| days > 0 && finished(h.run_state) && h.run_age_days >= days
            }),
        ),
    ];
    let keep = rules.keep_lineage_referenced;
    // Content that stays: every output no rule selects keeps its bytes.
    let mut selected: Vec<(usize, &Held)> = Vec::new();
    for (idx, (_, pick)) in table.iter().enumerate() {
        for h in &held {
            if pick(h)
                && !(keep && h.head)
                && !selected.iter().any(|(_, s)| s.artifact_id == h.artifact_id)
            {
                selected.push((idx, h));
            }
        }
    }
    let selected_ids: BTreeSet<&str> = selected
        .iter()
        .map(|(_, h)| h.artifact_id.as_str())
        .collect();
    let kept_content: BTreeSet<&str> = held
        .iter()
        .filter(|h| !selected_ids.contains(h.artifact_id.as_str()))
        .map(|h| h.blake3.as_str())
        .collect();
    let mut counted: BTreeSet<&str> = BTreeSet::new();
    for (idx, (label, _)) in table.iter().enumerate() {
        let mut line = Line {
            rule: label.clone(),
            ..Line::default()
        };
        for (i, h) in selected.iter().filter(|(i, _)| *i == idx) {
            let _ = i;
            line.artifacts += 1;
            // Bytes are freed only when no kept output has the same content.
            if !kept_content.contains(h.blake3.as_str()) && counted.insert(h.blake3.as_str()) {
                line.bytes += h.size;
            }
            remove.push((h.artifact_id.clone(), h.blake3.clone(), h.size));
        }
        if line.artifacts > 0 {
            lines.push(line);
        }
    }
    let caches = if rules.clear_caches_after_days > 0 {
        cache_entries(d, rules.clear_caches_after_days)
    } else {
        Vec::new()
    };
    let caches_bytes = caches
        .iter()
        .map(|p| {
            if p.is_dir() {
                dir_bytes(p)
            } else {
                std::fs::metadata(p).map_or(0, |m| m.len())
            }
        })
        .sum();
    Ok(Plan {
        remove,
        lines,
        caches,
        caches_bytes,
    })
}

fn info(d: &Daemon) -> ApiResult<Value> {
    let held = survey(d)?;
    let mut unique: BTreeMap<&str, u64> = BTreeMap::new();
    let mut by_wf: BTreeMap<&str, (u64, BTreeMap<&str, u64>)> = BTreeMap::new();
    for h in &held {
        unique.insert(&h.blake3, h.size);
        let e = by_wf.entry(&h.workflow).or_default();
        e.0 += 1;
        e.1.insert(&h.blake3, h.size);
    }
    let rules = rules_of(d);
    let mut runs = 0u64;
    let mut before = None;
    loop {
        let page = d.store.list_runs(before, 500).map_err(store_err)?;
        runs += page.len() as u64;
        let more = page.len() == 500;
        before = page.last().map(|r| r.id);
        if !more {
            break;
        }
    }
    let caches: u64 = cache_entries(d, 0)
        .iter()
        .map(|p| {
            if p.is_dir() {
                dir_bytes(p)
            } else {
                std::fs::metadata(p).map_or(0, |m| m.len())
            }
        })
        .sum();
    let limit = d
        .config
        .artifact_quota_bytes
        .or_else(|| (rules.limit_gib > 0).then(|| rules.limit_gib * GIB))
        .unwrap_or(0);
    let retention = rules.retention_days;
    Ok(json!({
        "root": rules.store_location,
        "artifacts": { "count": held.len(), "bytes": unique.values().sum::<u64>() },
        "runs": { "count": runs },
        "caches": { "bytes": caches },
        "limit_bytes": limit,
        "retention_days": retention,
        "by_workflow": by_wf.into_iter().map(|(w, (n, c))| json!({"workflow": w, "artifacts": n, "bytes": c.values().sum::<u64>()})).collect::<Vec<_>>(),
    }))
}

fn mark_removed(d: &Daemon, ids: &BTreeSet<String>) -> ApiResult<()> {
    let outs: Vec<(String, Vec<OutputRecord>)> = d
        .store
        .list_docs::<Vec<OutputRecord>>(Collection::Outputs, "")
        .map_err(store_err)?;
    for (key, mut list) in outs {
        let mut changed = false;
        for o in &mut list {
            if ids.contains(&o.artifact_id) && !o.removed {
                o.removed = true;
                changed = true;
            }
        }
        if changed {
            d.store
                .put_doc(Collection::Outputs, &key, &list)
                .map_err(store_err)?;
        }
    }
    Ok(())
}

fn cleanup(d: &Daemon) -> ApiResult<Value> {
    let p = plan(d)?;
    let ids: BTreeSet<String> = p.remove.iter().map(|(id, _, _)| id.clone()).collect();
    let mut removed = 0u64;
    for id in &ids {
        let Ok(aid) = id.parse() else { continue };
        match d.artifacts.delete(aid) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(error = %e, "cleanup could not remove an artifact record"),
        }
    }
    mark_removed(d, &ids)?;
    let swept = d.artifacts.collect_step(false, 10_000).unwrap_or(0);
    let mut caches_cleared = 0u64;
    for c in &p.caches {
        let r = if c.is_dir() {
            std::fs::remove_dir_all(c)
        } else {
            std::fs::remove_file(c)
        };
        if r.is_ok() {
            caches_cleared += 1;
        }
    }
    tracing::info!(
        artifacts = removed,
        objects = swept,
        caches = caches_cleared,
        "storage cleanup"
    );
    info(d)
}

#[derive(Deserialize)]
struct SetRules {
    patch: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
struct SetRetention {
    days: u32,
}

/// Routes `storage.*`.
pub fn dispatch(d: &Arc<Daemon>, conn: &Conn, method: &str, p: Value) -> Option<ApiResult<Value>> {
    Some(match method {
        "storage.info" => info(d),
        "storage.rules" => out(&rules_of(d)),
        "storage.set_rules" => args::<SetRules>(p).and_then(|s| set_rules(d, conn, &d.studio.docs, s.patch)),
        "storage.set_retention" => args::<SetRetention>(p).and_then(|s| {
            set_rules(
                d,
                conn,
                &d.studio.docs,
                serde_json::Map::from_iter([("retention_days".to_owned(), json!(s.days))]),
            )?;
            info(d)
        }),
        "storage.cleanup_plan" => plan(d).and_then(|p| {
            let artifacts: u64 = p.lines.iter().map(|l| l.artifacts).sum();
            let bytes: u64 = p.lines.iter().map(|l| l.bytes).sum();
            let mut lines: Vec<Value> = p
                .lines
                .iter()
                .map(|l| json!({"rule": l.rule, "artifacts": l.artifacts, "bytes": l.bytes}))
                .collect();
            if !p.caches.is_empty() {
                lines.push(json!({"rule": "Caches older than the cache rule", "artifacts": 0, "bytes": p.caches_bytes}));
            }
            Ok(json!({
                "artifacts": artifacts,
                "bytes": bytes,
                "caches_bytes": p.caches_bytes,
                "logs": 0,
                "lines": lines,
            }))
        }),
        "storage.cleanup" => {
            tracing::info!(by = %conn.label(), "storage cleanup requested");
            cleanup(d)
        }
        "storage.clear_caches" => {
            for c in cache_entries(d, 0) {
                let _ = if c.is_dir() { std::fs::remove_dir_all(&c) } else { std::fs::remove_file(&c) };
            }
            info(d)
        }
        _ => return None,
    })
}

fn set_rules(
    d: &Daemon,
    conn: &Conn,
    docs: &Docs,
    patch: serde_json::Map<String, Value>,
) -> ApiResult<Value> {
    let mut v = serde_json::to_value(rules_of(d)).map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(obj) = v.as_object_mut() {
        for (k, x) in patch {
            if k == "store_location" {
                return Err(ApiError::invalid_params(
                    "storage: the artifact store cannot be moved from here",
                ));
            }
            obj.insert(k, x);
        }
    }
    let next: StorageRules =
        serde_json::from_value(v).map_err(|e| ApiError::invalid_params(format!("storage: {e}")))?;
    for days in [
        next.retention_days,
        next.delete_failed_after_days,
        next.delete_cancelled_after_days,
        next.delete_partials_after_days,
        next.delete_logs_after_days,
        next.clear_caches_after_days,
    ] {
        if days > 3650 {
            return Err(ApiError::invalid_params(
                "storage: days are 0 (off) to 3650",
            ));
        }
    }
    if next.limit_gib > 1_000_000 {
        return Err(ApiError::invalid_params(
            "storage: limit_gib is at most 1000000",
        ));
    }
    docs.write("storage", &next)?;
    tracing::info!(by = %conn.label(), "storage rules changed");
    out(&rules_of(d))
}
