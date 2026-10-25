//! The closed command set the webview may invoke. Each command maps to exactly one daemon method with typed
//! parameters (the `ogun_api::methods` types, so malformed input is refused before it reaches the daemon); results
//! pass through as the daemon's JSON. The one command that takes a method name, `prealpha_call`, relays only the
//! closed list of the hosted pre-alpha service (`hub::PREALPHA_METHODS`), never a daemon method.

use ogun_api::methods::{
    ApprovalRef, CancelRunParams, Decision, EventsSinceParams, GetArtifactParams,
    GetWorkflowParams, LineageParams, ListApprovalsParams, ListArtifactsParams, ListRunsParams,
    PartitionStatusParams, PlanBackfillParams, PutWorkflowParams, RunRef, StartRunParams,
    StreamRef, WorkflowDocument, name,
};
use ogun_types::ApprovalId;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::State;
use zeroize::Zeroizing;

use crate::approvals::Prepared;
use crate::error::{StudioError, StudioResult};
use crate::hub::MAX_INLINE_BYTES;
use crate::link::{Studio, StudioInfo};

type Out = StudioResult<Value>;

fn to_value<T: Serialize>(p: &T) -> StudioResult<Value> {
    serde_json::to_value(p).map_err(|e| StudioError::invalid(e.to_string()))
}

/// Shell and link description.
#[tauri::command(rename_all = "snake_case")]
pub async fn studio_info(state: State<'_, Studio>) -> StudioResult<StudioInfo> {
    Ok(state.info().await)
}

/// Use the local daemon (desktop).
#[tauri::command(rename_all = "snake_case")]
pub async fn link_local(state: State<'_, Studio>) -> StudioResult<StudioInfo> {
    state.use_local().await?;
    Ok(state.info().await)
}

/// Use a hub, read-only, with an API token. The token stays in the Rust process.
#[tauri::command(rename_all = "snake_case")]
pub async fn link_hub(
    state: State<'_, Studio>,
    url: String,
    token: String,
    ca_pem: Option<String>,
    remember: bool,
) -> StudioResult<StudioInfo> {
    let token = Zeroizing::new(token);
    state
        .use_hub(
            &url,
            Zeroizing::new(token.trim().to_owned()),
            ca_pem,
            remember,
        )
        .await?;
    Ok(state.info().await)
}

/// Forget the saved hub.
#[tauri::command(rename_all = "snake_case")]
pub async fn link_forget(state: State<'_, Studio>) -> StudioResult<StudioInfo> {
    state.forget_hub().await;
    Ok(state.info().await)
}

/// One method of the hosted pre-alpha service (agent threads and the settings the daemon does not implement yet),
/// from the closed list in `hub::PREALPHA_METHODS`, over the hub link only.
#[tauri::command(rename_all = "snake_case")]
pub async fn prealpha_call(
    state: State<'_, Studio>,
    method: String,
    params: Value,
    install: String,
) -> Out {
    state.prealpha_call(&method, params, &install).await
}

/// `daemon.status`.
#[tauri::command(rename_all = "snake_case")]
pub async fn daemon_status(state: State<'_, Studio>) -> Out {
    state.call(name::DAEMON_STATUS, json!({})).await
}

/// `workflows.list`.
#[tauri::command(rename_all = "snake_case")]
pub async fn workflows_list(state: State<'_, Studio>) -> Out {
    state.call(name::WORKFLOWS_LIST, json!({})).await
}

/// `workflows.get`.
#[tauri::command(rename_all = "snake_case")]
pub async fn workflows_get(state: State<'_, Studio>, params: GetWorkflowParams) -> Out {
    state.call(name::WORKFLOWS_GET, to_value(&params)?).await
}

/// `workflows.validate`.
#[tauri::command(rename_all = "snake_case")]
pub async fn workflows_validate(state: State<'_, Studio>, params: WorkflowDocument) -> Out {
    state
        .call(name::WORKFLOWS_VALIDATE, to_value(&params)?)
        .await
}

/// `workflows.put` (local link only).
#[tauri::command(rename_all = "snake_case")]
pub async fn workflows_put(state: State<'_, Studio>, params: PutWorkflowParams) -> Out {
    state.call(name::WORKFLOWS_PUT, to_value(&params)?).await
}

/// `runs.list`.
#[tauri::command(rename_all = "snake_case")]
pub async fn runs_list(state: State<'_, Studio>, params: ListRunsParams) -> Out {
    state.call(name::RUNS_LIST, to_value(&params)?).await
}

/// `runs.get`.
#[tauri::command(rename_all = "snake_case")]
pub async fn runs_get(state: State<'_, Studio>, params: RunRef) -> Out {
    state.call(name::RUNS_GET, to_value(&params)?).await
}

/// `runs.start` (local link only).
#[tauri::command(rename_all = "snake_case")]
pub async fn runs_start(state: State<'_, Studio>, params: StartRunParams) -> Out {
    state.call(name::RUNS_START, to_value(&params)?).await
}

/// `runs.cancel` (local link only).
#[tauri::command(rename_all = "snake_case")]
pub async fn runs_cancel(state: State<'_, Studio>, params: CancelRunParams) -> Out {
    state.call(name::RUNS_CANCEL, to_value(&params)?).await
}

/// `partitions.status`.
#[tauri::command(rename_all = "snake_case")]
pub async fn partitions_status(state: State<'_, Studio>, params: PartitionStatusParams) -> Out {
    state
        .call(name::PARTITIONS_STATUS, to_value(&params)?)
        .await
}

/// `backfills.plan`.
#[tauri::command(rename_all = "snake_case")]
pub async fn backfills_plan(state: State<'_, Studio>, params: PlanBackfillParams) -> Out {
    state.call(name::BACKFILLS_PLAN, to_value(&params)?).await
}

/// `events.since`, long-polling up to 30 s.
#[tauri::command(rename_all = "snake_case")]
pub async fn events_since(state: State<'_, Studio>, params: EventsSinceParams) -> Out {
    let mut p = params;
    p.wait_ms = p.wait_ms.min(30_000);
    state.call(name::EVENTS_SINCE, to_value(&p)?).await
}

/// `approvals.list`.
#[tauri::command(rename_all = "snake_case")]
pub async fn approvals_list(state: State<'_, Studio>, params: ListApprovalsParams) -> Out {
    state.call(name::APPROVALS_LIST, to_value(&params)?).await
}

/// `approvals.get`.
#[tauri::command(rename_all = "snake_case")]
pub async fn approvals_get(state: State<'_, Studio>, params: ApprovalRef) -> Out {
    state.call(name::APPROVALS_GET, to_value(&params)?).await
}

/// Review a decision; returns the facts and a single-use confirmation id.
#[tauri::command(rename_all = "snake_case")]
pub async fn approvals_prepare(
    state: State<'_, Studio>,
    approval_id: ApprovalId,
    decision: Decision,
) -> StudioResult<Prepared> {
    state
        .confirmations
        .prepare(&state, approval_id, decision)
        .await
}

/// Send a reviewed decision with `studio` evidence.
#[tauri::command(rename_all = "snake_case")]
pub async fn approvals_confirm(state: State<'_, Studio>, confirmation_id: String) -> Out {
    let view = state
        .confirmations
        .confirm(&state, &confirmation_id)
        .await?;
    to_value(&view)
}

/// Drop a decision under review.
#[tauri::command(rename_all = "snake_case")]
pub async fn approvals_discard(
    state: State<'_, Studio>,
    confirmation_id: String,
) -> StudioResult<()> {
    state.confirmations.discard(&confirmation_id).await;
    Ok(())
}

/// `artifacts.list`.
#[tauri::command(rename_all = "snake_case")]
pub async fn artifacts_list(state: State<'_, Studio>, params: ListArtifactsParams) -> Out {
    state.call(name::ARTIFACTS_LIST, to_value(&params)?).await
}

/// `artifacts.get`, with inline bytes capped at 1 MiB for previews. The local `path` is not passed on.
#[tauri::command(rename_all = "snake_case")]
pub async fn artifacts_get(state: State<'_, Studio>, params: GetArtifactParams) -> Out {
    let mut p = params;
    p.inline_max_bytes = p.inline_max_bytes.min(MAX_INLINE_BYTES);
    let mut v = state.call(name::ARTIFACTS_GET, to_value(&p)?).await?;
    if let Some(obj) = v.as_object_mut() {
        obj.remove("path");
    }
    Ok(v)
}

/// `lineage.get`.
#[tauri::command(rename_all = "snake_case")]
pub async fn lineage_get(state: State<'_, Studio>, params: LineageParams) -> Out {
    state.call(name::LINEAGE_GET, to_value(&params)?).await
}

/// `streams.list`.
#[tauri::command(rename_all = "snake_case")]
pub async fn streams_list(state: State<'_, Studio>) -> Out {
    state.call(name::STREAMS_LIST, json!({})).await
}

/// `streams.status`.
#[tauri::command(rename_all = "snake_case")]
pub async fn streams_status(state: State<'_, Studio>, params: StreamRef) -> Out {
    state.call(name::STREAMS_STATUS, to_value(&params)?).await
}

/// `edges.list`.
#[tauri::command(rename_all = "snake_case")]
pub async fn edges_list(state: State<'_, Studio>) -> Out {
    state.call(name::EDGES_LIST, json!({})).await
}

/// `credentials.list` (names, kinds and dates; never tokens).
#[tauri::command(rename_all = "snake_case")]
pub async fn credentials_list(state: State<'_, Studio>) -> Out {
    state.call(name::CREDENTIALS_LIST, json!({})).await
}

/// `secrets.list` (names only).
#[tauri::command(rename_all = "snake_case")]
pub async fn secrets_list(state: State<'_, Studio>) -> Out {
    state.call(name::SECRETS_LIST, json!({})).await
}

/// The daemon's LLM provider ladder, read-only, when the daemon offers `llm.ladder`; otherwise
/// `{"available": false}`. Studio shows provider names, models and budgets, never keys.
#[tauri::command(rename_all = "snake_case")]
pub async fn llm_ladder(state: State<'_, Studio>) -> Out {
    Ok(match state.call_if_offered(LLM_LADDER, json!({})).await? {
        Some(v) => json!({ "available": true, "ladder": v }),
        None => json!({ "available": false }),
    })
}

/// Not yet in `ogun_api::methods::name`: the read-only ladder view the daemon may add with Ogun's LLM ladder.
const LLM_LADDER: &str = "llm.ladder";
