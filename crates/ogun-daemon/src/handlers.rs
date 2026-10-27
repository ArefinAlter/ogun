//! Method handlers (O-15 section 5). The router in [`crate::server`] has already applied the policy
//! table; handlers apply the rules the table cannot express.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ogun_api::methods::{
    AddCredentialParams, AddCredentialResult, ApprovalKind, ApprovalRef, ApprovalState,
    ApproverRef, ArtifactView, CancelRunParams, CredentialView, DecideParams, Diagnostic, EdgeView,
    Empty, EnrollEdgeParams, EventView, EventsSinceParams, EventsSinceResult, Fact,
    GetArtifactParams, GetArtifactResult, GetWorkflowParams, GetWorkflowResult, LineageParams,
    LineageResult, ListApprovalsParams, ListApprovalsResult, ListApproversResult,
    ListArtifactsParams, ListArtifactsResult, ListCredentialsResult, ListEdgesResult,
    ListRunsParams, ListRunsResult, ListSecretsResult, ListStreamsResult, ListWorkflowsResult,
    NodeView, PartitionRange, PartitionSelector, PartitionState, PartitionStatusParams,
    PartitionStatusResult, PendingApproval, PlanBackfillParams, PlanBackfillResult,
    ProposeApproverParams, PushParams, PutSecretParams, PutWorkflowParams, PutWorkflowResult,
    RevokeCredentialParams, RunRef, RunView, SecretRef, SetEnabledParams, StartRunParams,
    StatusResult, StreamRef, ValidateResult, WorkflowDocument, WorkflowSummary, name,
};
use ogun_api::{ApiError, ApiResult};
use ogun_ir::model::Partitions;
use ogun_ir::{ApprovalReason, Workflow};
use ogun_run::{NodeState, RunState, derive_run_state};
use ogun_store::{Collection, DocWrite, NodeInstance, RunRecord};
use ogun_types::{RunId, Timestamp};
use serde::Serialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::approvals::{self, Request};
use crate::error::{artifact_err, store_err};
use crate::server::{Conn, params, result};
use crate::state::{
    ApprovalDoc, Daemon, IdempotencyDoc, OutputRecord, VersionRow, WorkflowRow, lock, version_key,
};

const MAX_WORKFLOW_BYTES: usize = 1024 * 1024;
const MAX_INLINE_BYTES: u64 = 8 * 1024 * 1024;

/// Routes one authenticated call.
///
/// # Errors
///
/// The handler's error.
pub async fn dispatch(d: &Arc<Daemon>, conn: &Conn, method: &str, p: Value) -> ApiResult<Value> {
    match method {
        name::DAEMON_STATUS => result(&status(d)?),
        name::DAEMON_SHUTDOWN => {
            tracing::info!(by = %conn.label(), "shutdown requested");
            d.shutdown.send_replace(true);
            result(&Empty {})
        }
        name::WORKFLOWS_VALIDATE => result(&validate(d, &params::<WorkflowDocument>(p)?)),
        name::WORKFLOWS_PUT => {
            let p: PutWorkflowParams = params(p)?;
            let key = p.idempotency_key.clone();
            let mut hashed = p.clone();
            hashed.code.clear();
            idempotent(d, conn, method, &key, &hashed, || {
                result(&put_workflow(d, conn, &p)?)
            })
        }
        name::WORKFLOWS_LIST => result(&list_workflows(d)?),
        name::WORKFLOWS_GET => result(&get_workflow(d, &params(p)?)?),
        name::WORKFLOWS_SET_ENABLED => {
            let p: SetEnabledParams = params(p)?;
            let mut row = d.workflow_row(&p.name)?;
            row.enabled = p.enabled;
            d.store
                .put_doc(Collection::Workflows, &p.name, &row)
                .map_err(store_err)?;
            result(&summary(d, &row)?)
        }
        name::RUNS_START => {
            let p: StartRunParams = params(p)?;
            result(&start_run(d, &conn.label(), Some(conn), &p)?)
        }
        name::RUNS_GET => {
            let p: RunRef = params(p)?;
            result(&run_view(
                d,
                &d.store.run(p.run_id).map_err(store_err)?,
                true,
            )?)
        }
        name::RUNS_LIST => result(&list_runs(d, &params(p)?)?),
        name::RUNS_CANCEL => {
            let p: CancelRunParams = params(p)?;
            let key = p.idempotency_key.clone();
            idempotent(d, conn, method, &key, &p, || {
                result(&cancel_run(d, p.run_id)?)
            })
        }
        name::BACKFILLS_PLAN => result(&plan_backfill(d, &params(p)?)?),
        name::PARTITIONS_STATUS => result(&partition_status(d, &params(p)?)?),
        name::EVENTS_SINCE => result(&events_since(d, params(p)?).await?),
        name::APPROVALS_LIST => {
            let p: ListApprovalsParams = params(p)?;
            result(&ListApprovalsResult {
                approvals: approvals::list(d, p.state)?,
            })
        }
        name::APPROVALS_GET => {
            let p: ApprovalRef = params(p)?;
            let doc: ApprovalDoc = d
                .store
                .get_doc(Collection::Approvals, &p.approval_id.to_string())
                .map_err(store_err)?
                .ok_or_else(|| ApiError::new("approval.not_found", "no such approval"))?;
            result(&doc.view)
        }
        name::APPROVALS_DECIDE => {
            let p: DecideParams = params(p)?;
            result(&approvals::decide(d, conn, &p)?)
        }
        name::ARTIFACTS_LIST => result(&list_artifacts(d, &params(p)?)?),
        name::ARTIFACTS_GET => result(&get_artifact(d, &params(p)?)?),
        name::LINEAGE_GET => result(&lineage(d, &params(p)?)?),
        name::STREAMS_PUSH => {
            let p: PushParams = params(p)?;
            result(&d.streams.push(d, conn.credential, &p)?)
        }
        name::STREAMS_LIST => result(&ListStreamsResult {
            streams: d.streams.list(),
        }),
        name::STREAMS_STATUS => {
            let p: StreamRef = params(p)?;
            result(&d.streams.status(d, &p.workflow, &p.node)?)
        }
        name::CREDENTIALS_ADD => {
            let p: AddCredentialParams = params(p)?;
            let (c, path) = lock(&d.creds).add(&d.paths, &d.ids, &p.name, p.kind)?;
            tracing::info!(credential = %c.id, kind = %c.kind, by = %conn.label(), "credential added");
            result(&AddCredentialResult {
                credential: cred_view(&c),
                token_file: path.display().to_string(),
            })
        }
        name::CREDENTIALS_LIST => {
            let creds = lock(&d.creds).all().map(cred_view).collect();
            result(&ListCredentialsResult { credentials: creds })
        }
        name::CREDENTIALS_REVOKE => {
            let p: RevokeCredentialParams = params(p)?;
            let c = lock(&d.creds).revoke(&d.paths, p.credential_id)?;
            tracing::info!(credential = %c.id, by = %conn.label(), "credential revoked");
            result(&cred_view(&c))
        }
        name::SECRETS_PUT => {
            let p: PutSecretParams = params(p)?;
            let value = Zeroizing::new(p.value);
            d.secrets.put(&p.name, &value)?;
            tracing::info!(secret = %p.name, by = %conn.label(), "secret stored");
            result(&Empty {})
        }
        name::SECRETS_LIST => result(&ListSecretsResult {
            names: d.secrets.names(),
        }),
        name::SECRETS_DELETE => {
            let p: SecretRef = params(p)?;
            d.secrets.delete(&p.name)?;
            result(&Empty {})
        }
        name::APPROVERS_PROPOSE => {
            let p: ProposeApproverParams = params(p)?;
            result(&crate::webauthn::propose(d, &p)?)
        }
        name::APPROVERS_LIST => result(&ListApproversResult {
            approvers: crate::webauthn::list(d)?,
        }),
        name::APPROVERS_CONFIRM => {
            let p: ApproverRef = params(p)?;
            result(&crate::webauthn::confirm(
                d,
                &p.credential_id,
                p.fingerprint.as_deref(),
            )?)
        }
        name::APPROVERS_REVOKE => {
            let p: ApproverRef = params(p)?;
            result(&crate::webauthn::revoke(d, &p.credential_id)?)
        }
        name::EDGES_ENROLL => result(&enroll_edge(d, &params(p)?)?),
        name::EDGES_LIST => {
            let edges = d
                .store
                .list_docs::<EdgeView>(Collection::Edges, "")
                .map_err(store_err)?
                .into_iter()
                .map(|(_, e)| e)
                .collect();
            result(&ListEdgesResult { edges })
        }
        name::SYNC_OFFER | name::SYNC_CHUNK => crate::sync::handle(d, method, p),
        _ => match Box::pin(crate::studio::dispatch(d, conn, method, p)).await {
            Some(r) => r,
            None => Err(ApiError::new("api.method_not_found", "no such method")),
        },
    }
}

fn cred_view(c: &ogun_api::auth::StoredCredential) -> CredentialView {
    CredentialView {
        credential_id: c.id,
        kind: c.kind,
        name: c.name.clone(),
        created_at: c.created_at,
        revoked: c.revoked,
    }
}

fn valid_idempotency_key(k: &str) -> bool {
    (1..=128).contains(&k.len()) && k.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn params_hash(p: &impl Serialize) -> ApiResult<String> {
    let bytes = zeus_verify::canonical::to_jcs_bytes(p)
        .map_err(|_| ApiError::invalid_params("parameters are not canonicalisable"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn idem_key(conn: &Conn, method: &str, key: &str) -> String {
    format!(
        "{}/{method}/{}",
        conn.credential,
        &blake3::hash(key.as_bytes()).to_hex().as_str()[..32]
    )
}

/// Runs `f` once per `(credential, method, key)`; repeats return the first result.
fn idempotent(
    d: &Daemon,
    conn: &Conn,
    method: &str,
    key: &str,
    p: &impl Serialize,
    f: impl FnOnce() -> ApiResult<Value>,
) -> ApiResult<Value> {
    if !valid_idempotency_key(key) {
        return Err(ApiError::invalid_params(
            "idempotency_key must be 1 to 128 printable ASCII characters",
        ));
    }
    let hash = params_hash(p)?;
    let doc_key = idem_key(conn, method, key);
    if let Some(prev) = d
        .store
        .get_doc::<IdempotencyDoc>(Collection::Idempotency, &doc_key)
        .map_err(store_err)?
    {
        return if prev.params_blake3 == hash {
            Ok(prev.result)
        } else {
            Err(ApiError::new(
                "api.idempotency_conflict",
                "the key was used with other parameters",
            ))
        };
    }
    let value = f()?;
    d.store
        .put_doc(
            Collection::Idempotency,
            &doc_key,
            &IdempotencyDoc {
                params_blake3: hash,
                result: value.clone(),
                at: Timestamp::now(),
            },
        )
        .map_err(store_err)?;
    Ok(value)
}

fn status(d: &Daemon) -> ApiResult<StatusResult> {
    let workflows = d
        .store
        .list_docs::<WorkflowRow>(Collection::Workflows, "")
        .map_err(store_err)?
        .len() as u64;
    let active: BTreeSet<RunId> = d
        .store
        .active_instances()
        .map_err(store_err)?
        .into_iter()
        .map(|i| i.run)
        .collect();
    let pending = approvals::list(d, Some(ApprovalState::Pending))?.len() as u64;
    Ok(StatusResult {
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        started_at: d.started_at,
        profile: d.paths.profile.clone(),
        workflows,
        runs_active: active.len() as u64,
        approvals_pending: pending,
        streams: d.streams.list().len() as u64,
        last_event_seq: d.store.last_event_seq().map_err(store_err)?,
        scheduler_enabled: d.studio.runtime.scheduler_enabled(),
    })
}

fn diagnostics(r: &ogun_ir::Report) -> Vec<Diagnostic> {
    r.diagnostics.iter().map(Diagnostic::from).collect()
}

fn check(d: &Daemon, doc: &Value) -> ApiResult<ogun_ir::Report> {
    let text =
        serde_json::to_string(doc).map_err(|_| ApiError::invalid_params("workflow is not JSON"))?;
    if text.len() > MAX_WORKFLOW_BYTES {
        return Err(ApiError::new(
            "ir.too_large",
            "a workflow document is at most 1 MiB",
        ));
    }
    Ok(ogun_ir::check(&text, &d.catalogue))
}

fn validate(d: &Daemon, p: &WorkflowDocument) -> ValidateResult {
    match check(d, &p.workflow) {
        Ok(r) => ValidateResult {
            ok: r.is_valid(),
            workflow_hash: r.workflow_hash.as_ref().map(ToString::to_string),
            diagnostics: diagnostics(&r),
        },
        Err(e) => ValidateResult {
            ok: false,
            workflow_hash: None,
            diagnostics: vec![Diagnostic {
                severity: ogun_ir::Severity::Error,
                code: e.code().to_owned(),
                path: String::new(),
                message: e.message,
            }],
        },
    }
}

fn invalid(r: &ogun_ir::Report) -> ApiError {
    let mut e = ogun_types::OgunError::new("ir.invalid", "the workflow has errors");
    let mut details = serde_json::Map::new();
    details.insert(
        "diagnostics".into(),
        serde_json::to_value(diagnostics(r)).unwrap_or(Value::Null),
    );
    e = e.with_details(details);
    ApiError::from(e)
}

fn put_workflow(d: &Daemon, conn: &Conn, p: &PutWorkflowParams) -> ApiResult<PutWorkflowResult> {
    let report = check(d, &p.workflow)?;
    if !report.is_valid() {
        return Err(invalid(&report));
    }
    let wf: Workflow = report
        .workflow()
        .cloned()
        .ok_or_else(|| ApiError::internal("valid report without workflow"))?;
    let hash = report
        .workflow_hash
        .as_ref()
        .map(ToString::to_string)
        .ok_or_else(|| ApiError::internal("no hash"))?;
    let diags = diagnostics(&report);
    let name = wf.workflow.clone();
    let existing: Option<VersionRow> = d
        .store
        .get_doc(Collection::Versions, &version_key(&name, wf.version))
        .map_err(store_err)?;
    let row: Option<WorkflowRow> = d
        .store
        .get_doc(Collection::Workflows, &name)
        .map_err(store_err)?;
    if let Some(v) = existing {
        if v.workflow_hash != hash {
            return Err(ApiError::new(
                "ir.hash_mismatch",
                format!(
                    "version {} of {name} is already stored with another hash; increase the version",
                    wf.version
                ),
            ));
        }
        let row = row.ok_or_else(|| ApiError::internal("version without workflow row"))?;
        return Ok(PutWorkflowResult {
            workflow_id: row.workflow_id,
            name,
            version: wf.version,
            workflow_hash: hash,
            created: false,
            diagnostics: v.diagnostics.clone(),
            approvals: pending_of(d, &v)?,
        });
    }
    if let Some(r) = &row
        && wf.version <= r.latest_version
    {
        return Err(ApiError::new(
            "ir.invalid",
            format!(
                "version must be greater than the latest stored version, {}",
                r.latest_version
            ),
        ));
    }
    // Code bundles: verified by hash, stored once, read-only.
    let mut code_facts = Vec::new();
    let mut new_code = Vec::new();
    for node in &wf.nodes {
        let Some(code) = &node.code else { continue };
        let path = d.code_path(&code.blake3);
        if !path.exists() {
            let b64 = p.code.get(&code.blake3).ok_or_else(|| {
                ApiError::new(
                    "ir.invalid",
                    format!(
                        "code bundle {} for node {} was not provided",
                        code.blake3, node.id
                    ),
                )
            })?;
            let bytes = STANDARD
                .decode(b64)
                .map_err(|_| ApiError::invalid_params("code bundles are base64"))?;
            if blake3::hash(&bytes).to_hex().as_str() != code.blake3 {
                return Err(ApiError::new(
                    "ir.hash_mismatch",
                    format!("code bundle for node {} does not match its hash", node.id),
                ));
            }
            crate::creds::write_private(&path, &bytes)?;
        }
        let approved = d
            .store
            .get_doc::<Value>(
                Collection::CodeApprovals,
                &format!("{name}/{}", code.blake3),
            )
            .map_err(store_err)?
            .is_some();
        if !approved {
            code_facts.push(Fact {
                label: "code".into(),
                value: format!("node {} ({}) runs code {}", node.id, node.op, code.blake3),
            });
            new_code.push(code.blake3.clone());
        }
    }
    let mut writes = Vec::new();
    let mut needed = Vec::new();
    let mut request = |kind: ApprovalKind,
                       facts: Vec<Fact>,
                       code: Vec<String>,
                       writes: &mut Vec<DocWrite>|
     -> ApiResult<()> {
        let mut all = vec![
            Fact {
                label: "workflow".into(),
                value: format!("{name} version {}", wf.version),
            },
            Fact {
                label: "workflow_hash".into(),
                value: hash.clone(),
            },
        ];
        all.extend(facts);
        let (doc, w) = approvals::new_approval(
            d,
            Request {
                kind,
                workflow: name.clone(),
                workflow_hash: hash.clone(),
                run_id: None,
                node: None,
                instance: None,
                facts: all,
                code,
                requested_by: conn.label(),
            },
        )?;
        needed.push(PendingApproval {
            approval_id: doc.view.approval_id,
            kind,
        });
        writes.push(w);
        Ok(())
    };
    if !code_facts.is_empty() {
        request(ApprovalKind::CodeChange, code_facts, new_code, &mut writes)?;
    }
    if let Some(r) = &row {
        let prev = d.workflow(&r.latest_hash)?;
        let diff = ogun_ir::diff(&prev, &wf, &d.catalogue);
        let mut budget = Vec::new();
        let mut policy = Vec::new();
        for reason in &diff.approval_reasons {
            match reason {
                ApprovalReason::BudgetIncreased { path } => {
                    budget.push(Fact {
                        label: "budget".into(),
                        value: format!("{path} grew or became unlimited"),
                    });
                }
                ApprovalReason::ModelUseAdded { node } => {
                    policy.push(Fact {
                        label: "model".into(),
                        value: format!("node {node} now calls a model"),
                    });
                }
                ApprovalReason::CodeChanged { .. } | ApprovalReason::AlwaysApproveAdded { .. } => {}
            }
        }
        if !budget.is_empty() {
            request(ApprovalKind::Budget, budget, vec![], &mut writes)?;
        }
        if !policy.is_empty() {
            request(ApprovalKind::Policy, policy, vec![], &mut writes)?;
        }
    }
    let now = Timestamp::now();
    let version = VersionRow {
        name: name.clone(),
        version: wf.version,
        workflow_hash: hash.clone(),
        stored_at: now,
        approvals: needed.iter().map(|a| a.approval_id).collect(),
        diagnostics: diags.clone(),
    };
    let new_row = match row {
        Some(mut r) => {
            r.latest_version = wf.version;
            r.latest_hash = hash.clone();
            r.updated_at = now;
            r.description = wf.description.clone();
            r
        }
        None => WorkflowRow {
            workflow_id: d.ids.workflow_id(),
            name: name.clone(),
            latest_version: wf.version,
            latest_hash: hash.clone(),
            enabled: true,
            created_at: now,
            updated_at: now,
            last_fire_ms: None,
            description: wf.description.clone(),
        },
    };
    writes.push(
        DocWrite::put(
            Collection::Versions,
            version_key(&name, wf.version),
            &version,
        )
        .map_err(store_err)?,
    );
    writes.push(DocWrite::put(Collection::Workflows, &name, &new_row).map_err(store_err)?);
    d.store.put_workflow(&wf, &writes).map_err(store_err)?;
    tracing::info!(workflow = %name, version = wf.version, approvals = needed.len(), by = %conn.label(), "workflow stored");
    d.changed();
    Ok(PutWorkflowResult {
        workflow_id: new_row.workflow_id,
        name,
        version: wf.version,
        workflow_hash: hash,
        created: true,
        diagnostics: diags,
        approvals: needed,
    })
}

fn pending_of(d: &Daemon, v: &VersionRow) -> ApiResult<Vec<PendingApproval>> {
    let mut out = Vec::new();
    for id in &v.approvals {
        if let Some(a) = d
            .store
            .get_doc::<ApprovalDoc>(Collection::Approvals, &id.to_string())
            .map_err(store_err)?
            && a.view.state != ApprovalState::Approved
        {
            out.push(PendingApproval {
                approval_id: *id,
                kind: a.view.kind,
            });
        }
    }
    Ok(out)
}

fn summary(d: &Daemon, row: &WorkflowRow) -> ApiResult<WorkflowSummary> {
    let v = d.version_row(&row.name, Some(row.latest_version))?;
    Ok(WorkflowSummary {
        workflow_id: row.workflow_id,
        name: row.name.clone(),
        latest_version: row.latest_version,
        workflow_hash: row.latest_hash.clone(),
        enabled: row.enabled,
        runnable: d.runnable(&v),
        updated_at: row.updated_at,
        untrusted_description: row.description.clone(),
    })
}

fn list_workflows(d: &Daemon) -> ApiResult<ListWorkflowsResult> {
    let rows = d
        .store
        .list_docs::<WorkflowRow>(Collection::Workflows, "")
        .map_err(store_err)?;
    let workflows = rows
        .iter()
        .map(|(_, r)| summary(d, r))
        .collect::<ApiResult<_>>()?;
    Ok(ListWorkflowsResult { workflows })
}

fn get_workflow(d: &Daemon, p: &GetWorkflowParams) -> ApiResult<GetWorkflowResult> {
    let row = d.workflow_row(&p.name)?;
    let v = d.version_row(&p.name, p.version)?;
    let wf = d.workflow(&v.workflow_hash)?;
    let normal = ogun_ir::hash::normal_form(&wf)
        .map_err(|_| ApiError::internal("stored workflow has no normal form"))?;
    Ok(GetWorkflowResult {
        summary: summary(d, &row)?,
        version: v.version,
        workflow_hash: v.workflow_hash,
        workflow: normal,
    })
}

/// The partitions a selector names, for one workflow.
fn select_partitions(
    d: &Daemon,
    wf: &Workflow,
    hash: &str,
    sel: &PartitionSelector,
) -> ApiResult<Vec<String>> {
    let now = Timestamp::now().unix_millis();
    let part_err = |e: ogun_sched::PartitionError| ApiError::invalid_params(e.to_string());
    let Some(p) = &wf.partitions else {
        return match sel {
            PartitionSelector::Latest => Ok(vec![String::new()]),
            _ => Err(ApiError::invalid_params(
                "this workflow is not partitioned; use {\"kind\":\"latest\"}",
            )),
        };
    };
    if matches!(p, Partitions::Window(_)) && !matches!(sel, PartitionSelector::Keys { .. }) {
        return Err(ApiError::invalid_params(
            "stream workflows run per sealed window; name window keys to re-run",
        ));
    }
    match sel {
        PartitionSelector::Latest => {
            let all = ogun_sched::complete(p, now).map_err(part_err)?;
            all.last()
                .cloned()
                .map(|k| vec![k])
                .ok_or_else(|| ApiError::invalid_params("no partition is complete yet"))
        }
        PartitionSelector::Keys { keys } => {
            if keys.is_empty() || keys.len() > ogun_sched::partitions::MAX_PARTITIONS {
                return Err(ApiError::invalid_params("between 1 and 100 000 keys"));
            }
            for k in keys {
                let ok = match p {
                    Partitions::Window(_) => ogun_sched::civil::parse_window_key(k).is_some(),
                    _ => ogun_sched::range(p, k, k, now)
                        .map(|r| r.len() == 1)
                        .unwrap_or(false),
                };
                if !ok {
                    return Err(ApiError::invalid_params(
                        "a key is not a partition of this workflow",
                    ));
                }
            }
            Ok(keys.clone())
        }
        PartitionSelector::Range { from, to, force } => {
            let keys = ogun_sched::range(p, from, to, now).map_err(part_err)?;
            if *force {
                return Ok(keys);
            }
            // Partitions another run is working on right now are left to it: starting them again would run the
            // same work twice (a backfill resumed after a restart, then started again by hand).
            let (done, running, _) = partition_states(d, &wf.workflow, hash)?;
            Ok(ogun_sched::plan(&keys, &done)
                .to_run
                .into_iter()
                .filter(|k| !running.contains(k))
                .collect())
        }
    }
}

/// Starts a run. Shared by `runs.start` and the scheduler (which passes no connection).
///
/// # Errors
///
/// `workflow.not_found`, `ir.hash_mismatch`, `workflow.not_approved`, `api.invalid_params`.
pub fn start_run(
    d: &Daemon,
    requested_by: &str,
    conn: Option<&Conn>,
    p: &StartRunParams,
) -> ApiResult<RunView> {
    let idem = match conn {
        Some(c) => {
            if !valid_idempotency_key(&p.idempotency_key) {
                return Err(ApiError::invalid_params(
                    "idempotency_key must be 1 to 128 printable ASCII characters",
                ));
            }
            let key = idem_key(c, name::RUNS_START, &p.idempotency_key);
            let hash = params_hash(p)?;
            if let Some(prev) = d
                .store
                .get_doc::<IdempotencyDoc>(Collection::Idempotency, &key)
                .map_err(store_err)?
            {
                return if prev.params_blake3 == hash {
                    serde_json::from_value(prev.result)
                        .map_err(|_| ApiError::internal("stored result"))
                } else {
                    Err(ApiError::new(
                        "api.idempotency_conflict",
                        "the key was used with other parameters",
                    ))
                };
            }
            Some((key, hash))
        }
        None => None,
    };
    let v = d.version_row(&p.workflow, p.version)?;
    if let Some(h) = &p.workflow_hash
        && h != &v.workflow_hash
    {
        return Err(ApiError::new(
            "ir.hash_mismatch",
            "the version's hash differs from the one named",
        ));
    }
    if !d.runnable(&v) {
        let mut e = ogun_types::OgunError::new(
            "workflow.not_approved",
            "this version needs approvals a person has not granted",
        );
        let mut details = serde_json::Map::new();
        details.insert(
            "approvals".into(),
            serde_json::to_value(&v.approvals).unwrap_or(Value::Null),
        );
        e = e.with_details(details);
        return Err(e.into());
    }
    let wf = d.workflow(&v.workflow_hash)?;
    let partitions = select_partitions(d, &wf, &v.workflow_hash, &p.partitions)?;
    if partitions.is_empty() {
        return Err(ApiError::invalid_params(
            "every partition in the range is up to date or already being worked on; pass force to run it again",
        ));
    }
    let now = Timestamp::now();
    let run = d
        .store
        .create_run_with(&wf, &partitions, &d.ids, now, |run| {
            let Some((key, hash)) = &idem else {
                return Ok(vec![]);
            };
            let view = RunView {
                run_id: run.id,
                workflow: run.workflow.clone(),
                version: run.version,
                workflow_hash: run.workflow_hash.clone(),
                state: RunState::Pending,
                partition_count: run.partitions.len() as u64,
                created_at: run.created_at,
                nodes: vec![],
            };
            let doc = IdempotencyDoc {
                params_blake3: hash.clone(),
                result: serde_json::to_value(&view)
                    .map_err(|_| ogun_store::StoreError::Unhashable)?,
                at: now,
            };
            Ok(vec![DocWrite::put(
                Collection::Idempotency,
                key.clone(),
                &doc,
            )?])
        })
        .map_err(store_err)?;
    tracing::info!(run = %run.id, workflow = %run.workflow, partitions = run.partitions.len(), by = requested_by, "run created");
    d.changed();
    run_view(d, &run, false)
}

fn run_view(d: &Daemon, run: &RunRecord, with_nodes: bool) -> ApiResult<RunView> {
    let instances = d.store.instances(run.id).map_err(store_err)?;
    let states: Vec<NodeState> = instances.iter().map(|i| i.state).collect();
    Ok(RunView {
        run_id: run.id,
        workflow: run.workflow.clone(),
        version: run.version,
        workflow_hash: run.workflow_hash.clone(),
        state: derive_run_state(&states),
        partition_count: run.partitions.len() as u64,
        created_at: run.created_at,
        nodes: if with_nodes {
            instances
                .iter()
                .map(|i| {
                    let mut v = node_view(i);
                    if matches!(i.state, NodeState::Failed | NodeState::RetryScheduled) {
                        v.error = d
                            .store
                            .get_doc(Collection::Errors, &i.id.to_string())
                            .ok()
                            .flatten();
                    }
                    v
                })
                .collect()
        } else {
            vec![]
        },
    })
}

fn node_view(i: &NodeInstance) -> NodeView {
    NodeView {
        instance_id: i.id,
        node: i.node.clone(),
        partition: i.partition.clone(),
        state: i.state,
        attempt: i.attempt,
        last_cause: i.last_cause,
        error: None,
        updated_at: i.updated_at,
    }
}

fn list_runs(d: &Daemon, p: &ListRunsParams) -> ApiResult<ListRunsResult> {
    let limit = p.limit.unwrap_or(50).clamp(1, 500) as usize;
    let mut out = Vec::new();
    let mut before = p.before;
    loop {
        let page = d.store.list_runs(before, 500).map_err(store_err)?;
        let exhausted = page.len() < 500;
        for r in &page {
            before = Some(r.id);
            if p.workflow.as_ref().is_none_or(|w| w == &r.workflow) {
                out.push(run_view(d, r, false)?);
                if out.len() == limit {
                    return Ok(ListRunsResult {
                        next: Some(r.id),
                        runs: out,
                    });
                }
            }
        }
        if exhausted {
            return Ok(ListRunsResult {
                runs: out,
                next: None,
            });
        }
    }
}

/// Cancels every unfinished node instance of a run.
fn cancel_run(d: &Daemon, run_id: RunId) -> ApiResult<RunView> {
    let run = d.store.run(run_id).map_err(store_err)?;
    let now = Timestamp::now();
    for i in d.store.instances(run_id).map_err(store_err)? {
        if i.state.is_terminal() {
            continue;
        }
        if let Some(tx) = lock(&d.running).get(&i.id) {
            tx.send_replace(true);
        }
        if let Err(e) = d.store.transition(
            i.id,
            ogun_run::Cause::Cancelled,
            ogun_run::Actor::Runtime,
            now,
            None,
        ) {
            tracing::debug!(error = %e, "instance finished before cancel");
        }
    }
    d.changed();
    run_view(d, &run, false)
}

fn compress(keys: &[String], state_of: impl Fn(&str) -> PartitionState) -> Vec<PartitionRange> {
    let mut out: Vec<PartitionRange> = Vec::new();
    for k in keys {
        let s = state_of(k);
        match out.last_mut() {
            Some(last) if last.state == s => {
                last.to.clone_from(k);
                last.count += 1;
            }
            _ => out.push(PartitionRange {
                from: k.clone(),
                to: k.clone(),
                count: 1,
                state: s,
            }),
        }
    }
    out
}

/// Partition states for a workflow hash: done from the store, running and failed from runs.
fn partition_states(
    d: &Daemon,
    workflow: &str,
    hash: &str,
) -> ApiResult<(BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)> {
    let done = d.store.done_partitions(hash).map_err(store_err)?;
    let mut running = BTreeSet::new();
    let mut failed = BTreeSet::new();
    for r in d.store.list_runs(None, 2_000).map_err(store_err)? {
        if r.workflow != workflow || r.workflow_hash != hash {
            continue;
        }
        for i in d.store.instances(r.id).map_err(store_err)? {
            if done.contains(&i.partition) {
                continue;
            }
            if !i.state.is_terminal() {
                running.insert(i.partition.clone());
            } else if i.state == NodeState::Failed {
                failed.insert(i.partition.clone());
            }
        }
    }
    Ok((done, running, failed))
}

fn state_fn<'a>(
    done: &'a BTreeSet<String>,
    running: &'a BTreeSet<String>,
    failed: &'a BTreeSet<String>,
) -> impl Fn(&str) -> PartitionState + 'a {
    move |k| {
        if done.contains(k) {
            PartitionState::Done
        } else if running.contains(k) {
            PartitionState::Running
        } else if failed.contains(k) {
            PartitionState::Failed
        } else {
            PartitionState::Missing
        }
    }
}

fn plan_backfill(d: &Daemon, p: &PlanBackfillParams) -> ApiResult<PlanBackfillResult> {
    let v = d.version_row(&p.workflow, None)?;
    let wf = d.workflow(&v.workflow_hash)?;
    let part = wf
        .partitions
        .as_ref()
        .ok_or_else(|| ApiError::invalid_params("this workflow is not partitioned"))?;
    let keys = ogun_sched::range(part, &p.from, &p.to, Timestamp::now().unix_millis())
        .map_err(|e| ApiError::invalid_params(e.to_string()))?;
    let (done, running, failed) = partition_states(d, &p.workflow, &v.workflow_hash)?;
    let plan = ogun_sched::plan(&keys, &done);
    Ok(PlanBackfillResult {
        workflow_hash: v.workflow_hash,
        to_run: plan.to_run.len() as u64,
        up_to_date: plan.up_to_date.len() as u64,
        ranges: compress(&keys, state_fn(&done, &running, &failed)),
    })
}

fn partition_status(d: &Daemon, p: &PartitionStatusParams) -> ApiResult<PartitionStatusResult> {
    let v = d.version_row(&p.workflow, p.version)?;
    let wf = d.workflow(&v.workflow_hash)?;
    let (done, running, failed) = partition_states(d, &p.workflow, &v.workflow_hash)?;
    let now = Timestamp::now().unix_millis();
    let keys: Vec<String> = match &wf.partitions {
        None => vec![String::new()],
        Some(Partitions::Window(_)) => {
            let mut all: BTreeSet<String> = done
                .iter()
                .chain(&running)
                .chain(&failed)
                .cloned()
                .collect();
            if let Some(f) = &p.from {
                all.retain(|k| k >= f);
            }
            if let Some(t) = &p.to {
                all.retain(|k| k <= t);
            }
            all.into_iter().collect()
        }
        Some(part) => {
            let complete = ogun_sched::complete(part, now)
                .map_err(|e| ApiError::invalid_params(e.to_string()))?;
            match (&p.from, &p.to) {
                (None, None) => complete,
                (f, t) => {
                    let from = f.clone().or_else(|| complete.first().cloned());
                    let to = t.clone().or_else(|| complete.last().cloned());
                    match (from, to) {
                        (Some(a), Some(b)) => ogun_sched::range(part, &a, &b, now)
                            .map_err(|e| ApiError::invalid_params(e.to_string()))?,
                        _ => vec![],
                    }
                }
            }
        }
    };
    Ok(PartitionStatusResult {
        workflow_hash: v.workflow_hash,
        ranges: compress(&keys, state_fn(&done, &running, &failed)),
    })
}

fn event_view(e: ogun_store::Event) -> EventView {
    EventView {
        seq: e.seq,
        at: e.at,
        run_id: e.run,
        instance_id: e.instance,
        kind: serde_json::to_value(&e.kind).unwrap_or(Value::Null),
    }
}

async fn events_since(d: &Daemon, p: EventsSinceParams) -> ApiResult<EventsSinceResult> {
    let limit = p.limit.unwrap_or(100).clamp(1, 1_000) as usize;
    let wait = std::time::Duration::from_millis(p.wait_ms.min(30_000));
    let deadline = tokio::time::Instant::now() + wait;
    let mut rx = d.events.subscribe();
    loop {
        let mut events = Vec::new();
        let mut after = p.after;
        // Scan forward in pages so a run filter still finds events past unrelated ones.
        loop {
            let page = d.store.events_after(after, 1_000).map_err(store_err)?;
            let n = page.len();
            for e in page {
                after = e.seq;
                if p.run_id.is_none_or(|r| r == e.run) {
                    events.push(event_view(e));
                    if events.len() == limit {
                        break;
                    }
                }
            }
            if n < 1_000 || events.len() == limit {
                break;
            }
        }
        if !events.is_empty() || wait.is_zero() || tokio::time::Instant::now() >= deadline {
            let last_seq = events.last().map_or(after.max(p.after), |e| e.seq);
            return Ok(EventsSinceResult { events, last_seq });
        }
        if tokio::time::timeout_at(deadline, rx.changed())
            .await
            .is_err()
        {
            return Ok(EventsSinceResult {
                events,
                last_seq: after.max(p.after),
            });
        }
    }
}

fn artifact_view(run: &RunRecord, i: &NodeInstance, o: &OutputRecord) -> ArtifactView {
    ArtifactView {
        artifact_id: o.artifact_id.clone(),
        blake3: o.blake3.clone(),
        size_bytes: o.size_bytes,
        media_type: o.media_type.clone(),
        run_id: run.id,
        node: i.node.clone(),
        partition: i.partition.clone(),
        output: o.port.clone(),
        committed_at: o.committed_at,
    }
}

fn list_artifacts(d: &Daemon, p: &ListArtifactsParams) -> ApiResult<ListArtifactsResult> {
    let run = d.store.run(p.run_id).map_err(store_err)?;
    let mut artifacts = Vec::new();
    for i in d.store.instances(p.run_id).map_err(store_err)? {
        let outs: Vec<OutputRecord> = d
            .store
            .get_doc(Collection::Outputs, &i.id.to_string())
            .map_err(store_err)?
            .unwrap_or_default();
        artifacts.extend(
            outs.iter()
                .filter(|o| !o.removed)
                .map(|o| artifact_view(&run, &i, o)),
        );
    }
    Ok(ListArtifactsResult { artifacts })
}

fn get_artifact(d: &Daemon, p: &GetArtifactParams) -> ApiResult<GetArtifactResult> {
    let digest = zeus_types::Hex256::from_hex(&p.blake3)
        .map_err(|_| ApiError::invalid_params("blake3 must be 64 hex digits"))?;
    let found = d
        .artifacts
        .list(&zeus_artifact::ArtifactFilter {
            blake3: Some(digest),
            limit: 1,
            ..Default::default()
        })
        .map_err(|e| artifact_err(&e))?;
    let a = found
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::new("artifact.not_found", "no artifact with that digest"))?;
    let meta = |k: &str| {
        a.metadata
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let run_id: RunId = meta("ogun.run_id").parse().map_err(|_| {
        ApiError::new(
            "artifact.not_found",
            "that digest is not an Ogun run output",
        )
    })?;
    let path = d.artifacts.object_path(digest);
    let content_base64 =
        if p.inline_max_bytes > 0 && a.size_bytes <= p.inline_max_bytes.min(MAX_INLINE_BYTES) {
            std::fs::read(&path).ok().map(|b| STANDARD.encode(b))
        } else {
            None
        };
    let view = ArtifactView {
        artifact_id: a.id.to_string(),
        blake3: p.blake3.clone(),
        size_bytes: a.size_bytes,
        media_type: a
            .declared_media_type
            .clone()
            .or(a.media_type.clone())
            .unwrap_or_else(|| "application/octet-stream".into()),
        run_id,
        node: meta("ogun.node"),
        partition: meta("ogun.partition"),
        output: meta("ogun.output"),
        committed_at: a.created_at,
    };
    Ok(GetArtifactResult {
        artifact: view,
        path: path.display().to_string(),
        content_base64,
    })
}

fn lineage(d: &Daemon, p: &LineageParams) -> ApiResult<LineageResult> {
    let run_id = match (p.run_id, &p.blake3) {
        (Some(r), _) => r,
        (None, Some(b)) => {
            get_artifact(
                d,
                &GetArtifactParams {
                    blake3: b.clone(),
                    inline_max_bytes: 0,
                },
            )?
            .artifact
            .run_id
        }
        (None, None) => return Err(ApiError::invalid_params("name a run_id or a blake3")),
    };
    let run = d.store.run(run_id).map_err(store_err)?;
    let instances = d.store.instances(run_id).map_err(store_err)?;
    let mut outputs = BTreeMap::new();
    for i in &instances {
        let outs: Vec<OutputRecord> = d
            .store
            .get_doc(Collection::Outputs, &i.id.to_string())
            .map_err(store_err)?
            .unwrap_or_default();
        outputs.insert(i.id, outs);
    }
    Ok(LineageResult {
        events: crate::lineage::run_events(d, &run, &instances, &outputs),
    })
}

fn enroll_edge(d: &Daemon, p: &EnrollEdgeParams) -> ApiResult<EdgeView> {
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(p.public_key.trim_end_matches('='))
        .map_err(|_| ApiError::invalid_params("public_key is base64url"))?;
    if key.len() != 32 {
        return Err(ApiError::invalid_params(
            "an Ed25519 public key is 32 bytes",
        ));
    }
    if !ogun_api::auth::valid_name(&p.name) {
        return Err(ApiError::invalid_params(
            "edge names are 1 to 64 of [a-z0-9_-]",
        ));
    }
    let view = EdgeView {
        edge_id: d.ids.edge_id(),
        name: p.name.clone(),
        public_key: p.public_key.trim_end_matches('=').to_owned(),
        enrolled_at: Timestamp::now(),
        last_sync_at: None,
    };
    d.store
        .put_doc(Collection::Edges, &view.edge_id.to_string(), &view)
        .map_err(store_err)?;
    tracing::info!(edge = %view.edge_id, "edge enrolled");
    Ok(view)
}
