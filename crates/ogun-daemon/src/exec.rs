//! The executor (O-01 section 5, O-03): moves node instances through the state machine and runs
//! operator attempts.
//!
//! Each tick looks at every unfinished node instance:
//!
//! * `pending`: ready when every upstream node of the same run and partition succeeded; skipped
//!   (`not_needed`) when an upstream was skipped or a route sent it nothing; cancelled when an
//!   upstream failed or was cancelled.
//! * `ready`: asks a person first when the node's policy is `approval: always`, then dispatches when
//!   a slot is free (per-node concurrency within a run, and the daemon's global limit).
//! * `retry_scheduled`: back to `ready` when the back-off elapsed.
//!
//! An attempt runs through [`ogun_ops::Registry::run`]. Outputs are ingested into the artifact store
//! and committed with `outputs_committed` in one transaction, after routing: each output goes to the
//! first route whose predicate matches its metadata, or downstream when none does. Source operators
//! (`stream.*`) are never dispatched here; the stream driver completes them when a window seals.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ogun_api::methods::{ApprovalKind, Fact};
use ogun_api::{ApiError, ApiResult};
use ogun_ir::model::{ApprovalPolicy, Node, Partitions, Predicate};
use ogun_ir::{OpKind, Workflow, parse_op_ref};
use ogun_ops::{
    CodeFile, Context, InputArtifact, Invocation, OpEvent, Outcome, OutputFile, PartitionWindow,
};
use ogun_run::{Actor, Cause, NodeState};
use ogun_store::{Collection, DocWrite, NodeInstance, RunRecord};
use ogun_types::{NodeInstanceId, RunId, Timestamp};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

use crate::error::store_err;
use crate::state::{Daemon, OutputRecord, RetryDoc, lock};

/// Runs until shutdown.
pub async fn run(d: Arc<Daemon>) {
    let mut shutdown = d.shutdown.subscribe();
    loop {
        tokio::select! {
            () = d.wake.notified() => {}
            () = tokio::time::sleep(Duration::from_millis(1_000)) => {}
            _ = shutdown.changed() => break,
        }
        crate::approvals::sweep(&d);
        d.streams.poll(&d);
        if let Err(e) = tick(&d) {
            tracing::error!(error = %e, "executor tick failed");
        }
    }
    // Ask running attempts to stop; their instances are recovered on the next start.
    for tx in lock(&d.running).values() {
        tx.send_replace(true);
    }
}

fn kind(d: &Daemon, node: &Node) -> Option<OpKind> {
    parse_op_ref(&node.op)
        .and_then(|r| d.catalogue.get(r.name))
        .map(|s| s.kind)
}

/// Upstream node ids: the producers of every input, plus every node that routes to this one.
fn upstream(wf: &Workflow, node: &Node) -> (Vec<String>, Vec<String>) {
    let inputs: Vec<String> = node
        .inputs
        .iter()
        .filter_map(|i| i.split_once('.').map(|(n, _)| n.to_owned()))
        .collect();
    let routers: Vec<String> = wf
        .nodes
        .iter()
        .filter(|n| n.route.iter().any(|r| r.to == node.id))
        .map(|n| n.id.clone())
        .collect();
    (inputs, routers)
}

fn outputs_of(d: &Daemon, id: NodeInstanceId) -> ApiResult<Vec<OutputRecord>> {
    Ok(d.store
        .get_doc(Collection::Outputs, &id.to_string())
        .map_err(store_err)?
        .unwrap_or_default())
}

fn tick(d: &Arc<Daemon>) -> ApiResult<()> {
    let active = d.store.active_instances().map_err(store_err)?;
    let mut by_run: BTreeMap<RunId, Vec<NodeInstance>> = BTreeMap::new();
    for i in active {
        by_run.entry(i.run).or_default().push(i);
    }
    for (run_id, mine) in by_run {
        let run = d.store.run(run_id).map_err(store_err)?;
        let wf = d.workflow(&run.workflow_hash)?;
        let all = d.store.instances(run_id).map_err(store_err)?;
        let state_of: HashMap<(&str, &str), &NodeInstance> = all
            .iter()
            .map(|i| ((i.node.as_str(), i.partition.as_str()), i))
            .collect();
        let mut running: HashMap<String, u32> = HashMap::new();
        for i in all.iter().filter(|i| i.state == NodeState::Running) {
            *running.entry(i.node.clone()).or_default() += 1;
        }
        for ni in &mine {
            let Some(node) = wf.nodes.iter().find(|n| n.id == ni.node) else {
                continue;
            };
            if kind(d, node) == Some(OpKind::Source) {
                continue;
            }
            let result = match ni.state {
                NodeState::Pending => pending(d, &wf, node, ni, &state_of),
                NodeState::Ready => ready(d, &run, &wf, node, ni, &mut running),
                NodeState::RetryScheduled => retry(d, ni),
                _ => Ok(()),
            };
            if let Err(e) = result {
                tracing::warn!(instance = %ni.id, error = %e, "could not advance a node instance");
            }
        }
    }
    d.bump_events();
    Ok(())
}

fn pending(
    d: &Daemon,
    wf: &Workflow,
    node: &Node,
    ni: &NodeInstance,
    state_of: &HashMap<(&str, &str), &NodeInstance>,
) -> ApiResult<()> {
    let (inputs, routers) = upstream(wf, node);
    let deps: Vec<&NodeInstance> = inputs
        .iter()
        .chain(&routers)
        .filter_map(|n| state_of.get(&(n.as_str(), ni.partition.as_str())).copied())
        .collect();
    let now = Timestamp::now();
    if deps
        .iter()
        .any(|u| matches!(u.state, NodeState::Failed | NodeState::Cancelled))
    {
        d.store
            .transition(ni.id, Cause::Cancelled, Actor::Runtime, now, None)
            .map_err(store_err)?;
        return Ok(());
    }
    if deps.iter().any(|u| u.state == NodeState::Skipped) {
        d.store
            .transition(ni.id, Cause::NotNeeded, Actor::Runtime, now, None)
            .map_err(store_err)?;
        return Ok(());
    }
    if !deps.iter().all(|u| u.state == NodeState::Succeeded) {
        return Ok(());
    }
    if inputs.is_empty() && !routers.is_empty() {
        // A route target runs only when some router sent it items.
        let mut any = false;
        for r in &routers {
            if let Some(u) = state_of.get(&(r.as_str(), ni.partition.as_str())) {
                any |= outputs_of(d, u.id)?
                    .iter()
                    .any(|o| o.route.as_deref() == Some(node.id.as_str()));
            }
        }
        if !any {
            d.store
                .transition(ni.id, Cause::NotNeeded, Actor::Runtime, now, None)
                .map_err(store_err)?;
            return Ok(());
        }
    }
    d.store
        .transition(ni.id, Cause::InputsAvailable, Actor::Runtime, now, None)
        .map_err(store_err)?;
    Ok(())
}

fn ready(
    d: &Arc<Daemon>,
    run: &RunRecord,
    wf: &Arc<Workflow>,
    node: &Node,
    ni: &NodeInstance,
    running: &mut HashMap<String, u32>,
) -> ApiResult<()> {
    let now = Timestamp::now();
    if node.policy.approval == ApprovalPolicy::Always
        && d.store
            .get_doc::<Value>(Collection::Granted, &ni.id.to_string())
            .map_err(store_err)?
            .is_none()
    {
        let (_, write) = crate::approvals::new_approval(
            d,
            crate::approvals::Request {
                kind: ApprovalKind::Policy,
                workflow: run.workflow.clone(),
                workflow_hash: run.workflow_hash.clone(),
                run_id: Some(run.id),
                node: Some(node.id.clone()),
                instance: Some(ni.id),
                facts: vec![
                    Fact {
                        label: "node".into(),
                        value: format!("{} ({})", node.id, node.op),
                    },
                    Fact {
                        label: "partition".into(),
                        value: if ni.partition.is_empty() {
                            "(none)".into()
                        } else {
                            ni.partition.clone()
                        },
                    },
                    Fact {
                        label: "reason".into(),
                        value: "the node's policy asks for approval before every run".into(),
                    },
                ],
                code: node.code.iter().map(|c| c.blake3.clone()).collect(),
                requested_by: "ogund (policy)".into(),
            },
        )?;
        d.store
            .transition_with(
                ni.id,
                Cause::ApprovalRequired,
                Actor::Runtime,
                now,
                None,
                &[write],
            )
            .map_err(store_err)?;
        return Ok(());
    }
    let here = running.get(node.id.as_str()).copied().unwrap_or(0);
    if here >= node.policy.concurrency || lock(&d.running).len() >= d.studio.runtime.concurrency() {
        return Ok(());
    }
    let (ni, budget) = if kind(d, node) == Some(OpKind::Model) {
        match crate::llm_budget::dispatch(d, wf, node, ni)? {
            crate::llm_budget::Dispatch::Reserved(ni, amount) => (ni, Some(amount)),
            crate::llm_budget::Dispatch::Busy => return Ok(()),
            crate::llm_budget::Dispatch::Exhausted(limits) => {
                if node.policy.approval == ApprovalPolicy::OverBudget {
                    request_budget_approval(d, run, node, ni, &limits)?;
                } else {
                    // The state machine records failures from Running. No operator is launched.
                    let ni = d
                        .store
                        .transition(ni.id, Cause::Dispatched, Actor::Runtime, now, None)
                        .map_err(store_err)?;
                    fail(
                        d,
                        node,
                        &ni,
                        ogun_types::OgunError::new(
                            "run.budget_exhausted",
                            "no model budget remains in this run partition",
                        ),
                        false,
                        now,
                    )
                    .map_err(store_err)?;
                }
                return Ok(());
            }
        }
    } else {
        (
            d.store
                .transition(ni.id, Cause::Dispatched, Actor::Runtime, now, None)
                .map_err(store_err)?,
            None,
        )
    };
    *running.entry(node.id.clone()).or_default() += 1;
    let (tx, rx) = watch::channel(false);
    lock(&d.running).insert(ni.id, tx);
    let d2 = d.clone();
    let run = run.clone();
    let wf = wf.clone();
    let node_id = node.id.clone();
    tokio::spawn(async move {
        let id = ni.id;
        let partition = ni.partition.clone();
        if let Err(e) = attempt(&d2, &run, &wf, &node_id, ni, rx, budget).await {
            tracing::error!(instance = %id, error = %e, "attempt bookkeeping failed");
            // If execution or accounting failed unexpectedly, retain the worst-case bill.
            if budget.is_some() {
                let _ = crate::llm_budget::settle(&d2, run.id, &partition, id, None);
            }
        }
        lock(&d2.running).remove(&id);
        d2.changed();
    });
    Ok(())
}

fn request_budget_approval(
    d: &Daemon,
    run: &RunRecord,
    node: &Node,
    ni: &NodeInstance,
    limits: &crate::llm_budget::Limits,
) -> ApiResult<()> {
    let (_, write) = crate::approvals::new_approval(
        d,
        crate::approvals::Request {
            kind: ApprovalKind::Budget,
            workflow: run.workflow.clone(),
            workflow_hash: run.workflow_hash.clone(),
            run_id: Some(run.id),
            node: Some(node.id.clone()),
            instance: Some(ni.id),
            facts: vec![limits.fact()?, Fact {
                label: "reason".into(),
                value: "the next model call needs more budget; approve these raised micro-USD limits for this run partition".into(),
            }],
            code: vec![],
            requested_by: "ogund (budget)".into(),
        },
    )?;
    d.store
        .transition_with(
            ni.id,
            Cause::ApprovalRequired,
            Actor::Runtime,
            Timestamp::now(),
            None,
            &[write],
        )
        .map_err(store_err)?;
    Ok(())
}

fn retry(d: &Daemon, ni: &NodeInstance) -> ApiResult<()> {
    let due = d
        .store
        .get_doc::<RetryDoc>(Collection::Retries, &ni.id.to_string())
        .map_err(store_err)?
        .map_or(0, |r| r.due_ms);
    let now = Timestamp::now();
    if now.unix_millis() >= due {
        let del = DocWrite::delete(Collection::Retries, ni.id.to_string());
        d.store
            .transition_with(
                ni.id,
                Cause::BackoffElapsed,
                Actor::Runtime,
                now,
                None,
                &[del],
            )
            .map_err(store_err)?;
    }
    Ok(())
}

fn window_of(wf: &Workflow, partition: &str) -> Option<PartitionWindow> {
    let p = wf.partitions.as_ref()?;
    if let Partitions::Window(w) = p {
        let start = ogun_sched::civil::parse_window_key(partition)?;
        let size = i64::try_from(w.size_ms).ok()?;
        return Some(PartitionWindow {
            start_ms: start,
            end_ms: start + size,
        });
    }
    ogun_sched::window(p, partition).map(|w| PartitionWindow {
        start_ms: w.start_ms,
        end_ms: w.end_ms,
    })
}

fn inputs_for(
    d: &Daemon,
    run: RunId,
    wf: &Workflow,
    node: &Node,
    partition: &str,
) -> ApiResult<Vec<InputArtifact>> {
    let all = d.store.instances(run).map_err(store_err)?;
    let find = |n: &str| all.iter().find(|i| i.node == n && i.partition == partition);
    let mut out = Vec::new();
    let mut push = |port: String, o: &OutputRecord| -> ApiResult<()> {
        let digest = zeus_types::Hex256::from_hex(&o.blake3)
            .map_err(|_| ApiError::internal("stored digest"))?;
        out.push(InputArtifact {
            port,
            path: d.artifacts.object_path(digest),
            blake3: o.blake3.clone(),
            media_type: o.media_type.clone(),
            size_bytes: o.size_bytes,
            metadata: o.metadata.clone(),
        });
        Ok(())
    };
    for input in &node.inputs {
        let Some((up, port)) = input.split_once('.') else {
            continue;
        };
        let Some(u) = find(up) else { continue };
        for o in outputs_of(d, u.id)?
            .iter()
            .filter(|o| o.port == port && o.route.is_none())
        {
            push(input.clone(), o)?;
        }
    }
    for router in wf
        .nodes
        .iter()
        .filter(|n| n.route.iter().any(|r| r.to == node.id))
    {
        let Some(u) = find(&router.id) else { continue };
        for o in outputs_of(d, u.id)?
            .iter()
            .filter(|o| o.route.as_deref() == Some(node.id.as_str()))
        {
            push(format!("{}.{}", router.id, o.port), o)?;
        }
    }
    Ok(out)
}

/// The closed predicate set (O-02 section 5) over an output's metadata, media type and size.
#[must_use]
pub fn matches(p: &Predicate, meta: &Value, media_type: &str, size: u64) -> bool {
    match p {
        Predicate::All(ps) => ps.iter().all(|q| matches(q, meta, media_type, size)),
        Predicate::Any(ps) => ps.iter().any(|q| matches(q, meta, media_type, size)),
        Predicate::Not(q) => !matches(q, meta, media_type, size),
        Predicate::Eq { field, value } => meta.get(field) == Some(value),
        Predicate::In { field, values } => meta.get(field).is_some_and(|v| values.contains(v)),
        Predicate::Exists { field } => meta.get(field).is_some_and(|v| !v.is_null()),
        Predicate::MediaType(pattern) => match pattern.strip_suffix("/*") {
            Some(top) => media_type.split('/').next() == Some(top),
            None => media_type.eq_ignore_ascii_case(pattern),
        },
        Predicate::SizeBytes { gte, lt } => {
            gte.is_none_or(|g| size >= g) && lt.is_none_or(|l| size < l)
        }
    }
}

fn within(scratch: &Path, p: &Path) -> Option<PathBuf> {
    let root = scratch.canonicalize().ok()?;
    let c = p.canonicalize().ok()?;
    c.starts_with(&root).then_some(c)
}

fn backoff_ms(attempt: u32) -> u64 {
    (1_000u64 << attempt.min(8)).min(300_000)
}

/// Workflow configuration cannot forge a reservation, including on non-model operators.
fn invocation_config(node: &Node, budget: Option<u64>) -> Value {
    let mut config: serde_json::Map<String, Value> = node.config.clone().into_iter().collect();
    config.remove("llm_budget_usd_micro");
    if let Some(amount) = budget {
        config.insert("llm_budget_usd_micro".into(), Value::from(amount));
    }
    Value::Object(config)
}

fn settlement_cost(outcome: &Outcome, reported: u64, reserved: u64) -> u64 {
    let uncertain = matches!(outcome, Outcome::Cancelled)
        || matches!(outcome, Outcome::Failed { error, .. } if matches!(error.code.as_str(),
            "op.timeout" | "op.crashed" | "op.protocol_violation" | "op.sandbox_violation"
                | "op.operator_failed" | "op.operator_transient" | "internal.bug"
                | "llm.provider_unavailable"));
    if uncertain {
        reported.max(reserved)
    } else {
        reported
    }
}

async fn attempt(
    d: &Arc<Daemon>,
    run: &RunRecord,
    wf: &Arc<Workflow>,
    node_id: &str,
    ni: NodeInstance,
    cancel: watch::Receiver<bool>,
    budget: Option<u64>,
) -> ApiResult<()> {
    let node = wf
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .ok_or_else(|| ApiError::internal("node vanished"))?;
    let scratch = d
        .paths
        .scratch_dir()
        .join(run.id.to_string())
        .join(ni.id.to_string())
        .join(ni.attempt.to_string());
    let _ = std::fs::remove_dir_all(&scratch);
    crate::creds::private_dir(&scratch)?;
    let code = match &node.code {
        Some(c) => {
            let path = d.code_path(&c.blake3);
            let ok =
                std::fs::read(&path).is_ok_and(|b| blake3::hash(&b).to_hex().as_str() == c.blake3);
            if !ok {
                crate::llm_budget::settle(d, run.id, &ni.partition, ni.id, Some(0))?;
                return finish(
                    d,
                    run,
                    node,
                    &ni,
                    &scratch,
                    Outcome::Failed {
                        error: ogun_types::OgunError::new(
                            "op.sandbox_violation",
                            "the code bundle is missing or does not match its hash",
                        ),
                        retryable: false,
                    },
                )
                .await;
            }
            Some(CodeFile {
                blake3: c.blake3.clone(),
                path,
            })
        }
        None => None,
    };
    let inputs = inputs_for(d, run.id, wf, node, &ni.partition)?;
    let inv = Invocation {
        run_id: run.id,
        node_instance: ni.id,
        node_id: node.id.clone(),
        partition: ni.partition.clone(),
        window: window_of(wf, &ni.partition),
        op: node.op.clone(),
        config: invocation_config(node, budget),
        inputs,
        scratch: scratch.clone(),
        checkpoint: ni.checkpoint.clone(),
        timeout_ms: node.policy.timeout_ms,
        code,
        attempt: ni.attempt,
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let ctx = Context {
        secrets: d.secrets.clone(),
        events: tx,
        cancel,
    };
    let events = {
        let d = d.clone();
        let id = ni.id;
        let run_id = run.id;
        tokio::spawn(async move {
            let mut total = 0u64;
            while let Some(ev) = rx.recv().await {
                match ev {
                    OpEvent::Checkpoint(v) => {
                        if let Err(e) = d.store.set_checkpoint(id, v) {
                            tracing::warn!(instance = %id, error = %e, "checkpoint refused");
                        }
                    }
                    OpEvent::Usage { cost_usd_micro, .. } => {
                        total = total.saturating_add(cost_usd_micro);
                        if budget.is_some_and(|b| total > b)
                            && let Some(tx) = lock(&d.running).get(&id)
                        {
                            tracing::warn!(run = %run_id, "model budget exhausted; cancelling the attempt");
                            tx.send_replace(true);
                        }
                    }
                    OpEvent::Log(line) => tracing::info!(instance = %id, "{line}"),
                    OpEvent::Progress { .. } => {}
                }
            }
            total
        })
    };
    let timeout = Duration::from_millis(node.policy.timeout_ms);
    let outcome = match tokio::time::timeout(timeout, d.registry.run(inv, ctx)).await {
        Ok(o) => o,
        Err(_) => Outcome::Failed {
            error: ogun_types::OgunError::new(
                "op.timeout",
                "the attempt exceeded policy.timeout_ms",
            ),
            retryable: true,
        },
    };
    let total = events
        .await
        .map_err(|_| ApiError::internal("model usage accounting task failed"))?;
    let outcome = if budget.is_some_and(|limit| total > limit) {
        Outcome::Failed {
            error: ogun_types::OgunError::new(
                "run.budget_exhausted",
                "model usage exceeded the reserved budget",
            ),
            retryable: false,
        }
    } else {
        outcome
    };
    if let Some(reserved) = budget {
        crate::llm_budget::settle(
            d,
            run.id,
            &ni.partition,
            ni.id,
            Some(settlement_cost(&outcome, total, reserved)),
        )?;
    }
    finish(d, run, node, &ni, &scratch, outcome).await
}

async fn finish(
    d: &Arc<Daemon>,
    run: &RunRecord,
    node: &Node,
    ni: &NodeInstance,
    scratch: &Path,
    outcome: Outcome,
) -> ApiResult<()> {
    let now = Timestamp::now();
    let id = ni.id;
    let current = d.store.instance(id).map_err(store_err)?;
    if current.state != NodeState::Running {
        // Cancelled (or recovered) while the operator ran; its result is dropped.
        let _ = std::fs::remove_dir_all(scratch);
        return Ok(());
    }
    let res = match outcome {
        Outcome::Succeeded { outputs } => {
            let d2 = d.clone();
            let run2 = run.clone();
            let node2 = node.clone();
            let ni2 = ni.clone();
            let scratch2 = scratch.to_path_buf();
            let committed = tokio::task::spawn_blocking(move || {
                commit(&d2, &run2, &node2, &ni2, &scratch2, &outputs)
            })
            .await
            .map_err(|_| ApiError::internal("commit task panicked"))?;
            match committed {
                Ok(records) => {
                    let w = DocWrite::put(Collection::Outputs, id.to_string(), &records)
                        .map_err(store_err)?;
                    d.store
                        .transition_with(
                            id,
                            Cause::OutputsCommitted,
                            Actor::Runtime,
                            now,
                            None,
                            &[w],
                        )
                        .map(|_| ())
                }
                Err(e) => fail(d, node, ni, e.data, false, now),
            }
        }
        Outcome::Failed { error, .. }
            if error.code == "run.budget_exhausted"
                && node.policy.approval == ApprovalPolicy::OverBudget =>
        {
            let wf = d.workflow(&run.workflow_hash)?;
            let limits = crate::llm_budget::raised_limits(d, &wf, node, ni)?;
            request_budget_approval(d, run, node, ni, &limits)?;
            Ok(())
        }
        Outcome::Failed { error, retryable } => fail(d, node, ni, error, retryable, now),
        Outcome::SessionRequired { request } => {
            let doc = crate::session::SessionDoc {
                request,
                job_id: None,
                zeus_approval_id: None,
            };
            let w = DocWrite::put(Collection::Sessions, id.to_string(), &doc).map_err(store_err)?;
            d.store
                .transition_with(id, Cause::SessionRequired, Actor::Runtime, now, None, &[w])
                .map(|_| ())
        }
        Outcome::Cancelled => d
            .store
            .transition(id, Cause::Cancelled, Actor::Runtime, now, None)
            .map(|_| ()),
    };
    let _ = std::fs::remove_dir_all(scratch);
    res.map_err(store_err)?;
    d.changed();
    Ok(())
}

fn fail(
    d: &Daemon,
    node: &Node,
    ni: &NodeInstance,
    error: ogun_types::OgunError,
    retryable: bool,
    now: Timestamp,
) -> Result<(), ogun_store::StoreError> {
    tracing::info!(instance = %ni.id, code = %error.code, message = %error.message, retryable, attempt = ni.attempt, "attempt failed");
    let err = DocWrite::put(Collection::Errors, ni.id.to_string(), &error)?;
    if retryable && ni.attempt <= node.policy.retries {
        let due = now
            .unix_millis()
            .saturating_add(i64::try_from(backoff_ms(ni.attempt)).unwrap_or(i64::MAX));
        let w = DocWrite::put(
            Collection::Retries,
            ni.id.to_string(),
            &RetryDoc { due_ms: due },
        )?;
        d.store
            .transition_with(
                ni.id,
                Cause::RetryableError,
                Actor::Operator,
                now,
                None,
                &[w, err],
            )
            .map(|_| ())
    } else {
        d.store
            .transition_with(
                ni.id,
                Cause::PermanentError,
                Actor::Operator,
                now,
                None,
                &[err],
            )
            .map(|_| ())
    }
}

/// Ingests an attempt's outputs and routes them. Blocking.
fn commit(
    d: &Daemon,
    run: &RunRecord,
    node: &Node,
    ni: &NodeInstance,
    scratch: &Path,
    outputs: &[OutputFile],
) -> ApiResult<Vec<OutputRecord>> {
    let mut records = Vec::with_capacity(outputs.len());
    for o in outputs {
        let path = within(scratch, &o.path).ok_or_else(|| {
            ApiError::new(
                "op.sandbox_violation",
                "an output is outside the attempt's scratch directory",
            )
        })?;
        let mut draft = zeus_artifact::ArtifactDraft::local(
            "ogun",
            env!("CARGO_PKG_VERSION"),
            path.file_name().and_then(|n| n.to_str()).map(str::to_owned),
        );
        draft.record.declared_media_type = Some(o.media_type.clone());
        for (k, v) in [
            ("ogun.workflow", run.workflow.clone()),
            ("ogun.workflow_hash", run.workflow_hash.clone()),
            ("ogun.run_id", run.id.to_string()),
            ("ogun.node", node.id.clone()),
            ("ogun.op", node.op.clone()),
            ("ogun.partition", ni.partition.clone()),
            ("ogun.output", o.port.clone()),
            ("ogun.node_instance", ni.id.to_string()),
        ] {
            draft.record.metadata.insert(k.into(), Value::String(v));
        }
        let artifact = d
            .artifacts
            .ingest(&path, draft, None)
            .map_err(|e| crate::error::artifact_err(&e))?;
        let meta = o.metadata.clone().unwrap_or(Value::Null);
        let route = node
            .route
            .iter()
            .find(|r| matches(&r.when, &meta, &o.media_type, artifact.size_bytes))
            .map(|r| r.to.clone());
        records.push(OutputRecord {
            port: o.port.clone(),
            blake3: artifact.blake3.to_string(),
            size_bytes: artifact.size_bytes,
            media_type: o.media_type.clone(),
            metadata: o.metadata.clone(),
            route,
            artifact_id: artifact.id.to_string(),
            committed_at: Timestamp::now(),
            removed: false,
        });
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn interrupted_and_unknown_operator_bills_retain_the_reserved_bound() {
        for code in [
            "op.timeout",
            "op.crashed",
            "op.protocol_violation",
            "op.sandbox_violation",
            "op.operator_failed",
            "op.operator_transient",
            "internal.bug",
            "llm.provider_unavailable",
        ] {
            let outcome = Outcome::Failed {
                error: ogun_types::OgunError::new(code, "unknown bill"),
                retryable: true,
            };
            assert_eq!(settlement_cost(&outcome, 20, 60), 60, "{code}");
            assert_eq!(settlement_cost(&outcome, 80, 60), 80, "overrun: {code}");
        }
        assert_eq!(settlement_cost(&Outcome::Cancelled, 20, 60), 60);
        assert_eq!(
            settlement_cost(&Outcome::Succeeded { outputs: vec![] }, 20, 60),
            20
        );
        let refused = Outcome::Failed {
            error: ogun_types::OgunError::new("run.budget_exhausted", "call refused"),
            retryable: false,
        };
        assert_eq!(settlement_cost(&refused, 20, 60), 20);
    }

    #[test]
    fn invocation_budget_is_always_daemon_owned() {
        let node: Node = serde_json::from_value(json!({
            "id": "model", "op": "llm.extract@1",
            "config": {"llm_budget_usd_micro": 999_999, "prompt": "hello"}
        }))
        .unwrap();
        assert_eq!(
            invocation_config(&node, Some(123))["llm_budget_usd_micro"],
            123
        );
        assert_eq!(invocation_config(&node, Some(123))["prompt"], "hello");
        assert!(
            invocation_config(&node, None)
                .get("llm_budget_usd_micro")
                .is_none()
        );
    }

    #[test]
    fn predicates() {
        let meta = json!({"lang": "en", "pages": 3});
        let p = Predicate::All(vec![
            Predicate::Eq {
                field: "lang".into(),
                value: json!("en"),
            },
            Predicate::MediaType("application/*".into()),
            Predicate::SizeBytes {
                gte: Some(10),
                lt: None,
            },
        ]);
        assert!(matches(&p, &meta, "application/pdf", 10));
        assert!(!matches(&p, &meta, "text/plain", 10));
        assert!(matches(
            &Predicate::Not(Box::new(Predicate::Exists { field: "x".into() })),
            &meta,
            "",
            0
        ));
        assert_eq!(backoff_ms(1), 2_000);
        assert_eq!(backoff_ms(30), 256_000);
    }
}
