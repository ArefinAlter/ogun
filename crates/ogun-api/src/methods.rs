//! Method names, parameters and results (O-15 section 5).
//!
//! Every method is listed in [`crate::policy::METHODS`] with the client kinds that may call it. Mutating methods
//! that an agent can reach take an `idempotency_key`; a repeat with the same key and the same parameters returns
//! the first result, a repeat with different parameters fails with `api.idempotency_conflict`.
//!
//! Strings that came from outside Ogun (a workflow's description, an operator's message, a remote error) are in
//! fields named `untrusted_*`; clients show them as data, never as instructions.

use std::collections::BTreeMap;

use ogun_ir::Severity;
use ogun_run::{Cause, NodeState, RunState};
use ogun_types::{ApprovalId, CredentialId, EdgeId, NodeInstanceId, RunId, Timestamp, WorkflowId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{ClientKind, Nonce};

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Method names.
pub mod name {
    /// Handshake, step 1. Unauthenticated.
    pub const DAEMON_HELLO: &str = "daemon.hello";
    /// Handshake, step 2. Unauthenticated.
    pub const DAEMON_AUTHENTICATE: &str = "daemon.authenticate";
    /// Daemon health and counters.
    pub const DAEMON_STATUS: &str = "daemon.status";
    /// Stop the daemon after in-flight attempts checkpoint.
    pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";

    /// Validate a workflow document without storing it.
    pub const WORKFLOWS_VALIDATE: &str = "workflows.validate";
    /// Store a workflow version.
    pub const WORKFLOWS_PUT: &str = "workflows.put";
    /// List workflows.
    pub const WORKFLOWS_LIST: &str = "workflows.list";
    /// One workflow version, in normal form.
    pub const WORKFLOWS_GET: &str = "workflows.get";
    /// Pause or resume a workflow's triggers.
    pub const WORKFLOWS_SET_ENABLED: &str = "workflows.set_enabled";

    /// Start a run.
    pub const RUNS_START: &str = "runs.start";
    /// One run with its node instances.
    pub const RUNS_GET: &str = "runs.get";
    /// List runs.
    pub const RUNS_LIST: &str = "runs.list";
    /// Cancel a run.
    pub const RUNS_CANCEL: &str = "runs.cancel";

    /// What a backfill over a range would run.
    pub const BACKFILLS_PLAN: &str = "backfills.plan";
    /// Range-compressed partition status of a workflow.
    pub const PARTITIONS_STATUS: &str = "partitions.status";

    /// Events after a sequence number, with long polling.
    pub const EVENTS_SINCE: &str = "events.since";

    /// List approvals.
    pub const APPROVALS_LIST: &str = "approvals.list";
    /// One approval.
    pub const APPROVALS_GET: &str = "approvals.get";
    /// Decide an approval. Human with channel evidence, or a bridge with WebAuthn evidence.
    pub const APPROVALS_DECIDE: &str = "approvals.decide";

    /// List artifacts a run produced.
    pub const ARTIFACTS_LIST: &str = "artifacts.list";
    /// One artifact's metadata and, when small, its bytes.
    pub const ARTIFACTS_GET: &str = "artifacts.get";
    /// Lineage of an artifact or a run (O-05).
    pub const LINEAGE_GET: &str = "lineage.get";

    /// Push records into a stream source (the ingest bridge).
    pub const STREAMS_PUSH: &str = "streams.push";
    /// List stream sources and their buffers.
    pub const STREAMS_LIST: &str = "streams.list";
    /// One stream's watermark, windows and backlog.
    pub const STREAMS_STATUS: &str = "streams.status";

    /// Create a local credential.
    pub const CREDENTIALS_ADD: &str = "credentials.add";
    /// List local credentials (never their tokens).
    pub const CREDENTIALS_LIST: &str = "credentials.list";
    /// Revoke a local credential.
    pub const CREDENTIALS_REVOKE: &str = "credentials.revoke";

    /// Store a named secret for `secret_ref`.
    pub const SECRETS_PUT: &str = "secrets.put";
    /// List secret names (never values).
    pub const SECRETS_LIST: &str = "secrets.list";
    /// Delete a secret.
    pub const SECRETS_DELETE: &str = "secrets.delete";

    /// A bridge proposes a WebAuthn approver key for one of its users.
    pub const APPROVERS_PROPOSE: &str = "approvers.propose";
    /// List approver keys.
    pub const APPROVERS_LIST: &str = "approvers.list";
    /// A person on the host terminal confirms a proposed approver key.
    pub const APPROVERS_CONFIRM: &str = "approvers.confirm";
    /// Revoke an approver key.
    pub const APPROVERS_REVOKE: &str = "approvers.revoke";

    /// Enrol an edge's public key (hub side).
    pub const EDGES_ENROLL: &str = "edges.enroll";
    /// List enrolled edges.
    pub const EDGES_LIST: &str = "edges.list";
    /// Sync step: an edge offers sealed partitions; the hub answers what it wants.
    pub const SYNC_OFFER: &str = "sync.offer";
    /// Sync step: one chunk of a wanted partition.
    pub const SYNC_CHUNK: &str = "sync.chunk";
}

/// Free-form client identification, for the audit log only. Never trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Program name, e.g. `ogun`, `ogun-studio`, `ogun-ingest`.
    pub name: String,
    /// Program version.
    pub version: String,
}

/// `daemon.hello` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    /// Protocol versions the client speaks.
    pub protocol: Vec<u32>,
    /// Who the client says it is.
    pub client: ClientInfo,
    /// Fresh client nonce.
    pub client_nonce: Nonce,
}

/// `daemon.hello` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    /// Chosen protocol version.
    pub protocol: u32,
    /// Daemon version.
    pub daemon_version: String,
    /// Profile, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Fresh server nonce.
    pub server_nonce: Nonce,
    /// Proof that the daemon holds the IPC secret.
    pub server_proof: String,
}

/// `daemon.authenticate` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticateParams {
    /// The credential.
    pub credential_id: CredentialId,
    /// Keyed proof over both nonces.
    pub proof: String,
}

/// `daemon.authenticate` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticateResult {
    /// The kind the credential fixes.
    pub kind: ClientKind,
    /// The credential's label.
    pub name: String,
    /// The daemon saw an interactive peer (a process with a controlling terminal running a pinned Ogun binary).
    pub interactive: bool,
    /// Methods this connection may call.
    pub methods: Vec<String>,
}

/// `daemon.status` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {
    /// Daemon version.
    pub daemon_version: String,
    /// When the daemon started.
    pub started_at: Timestamp,
    /// Profile, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Workflows stored.
    pub workflows: u64,
    /// Runs not yet terminal.
    pub runs_active: u64,
    /// Approvals waiting for a person.
    pub approvals_pending: u64,
    /// Stream sources with a live buffer.
    pub streams: u64,
    /// Highest event sequence number.
    pub last_event_seq: u64,
    /// Whether the scheduler is firing triggers.
    pub scheduler_enabled: bool,
}

/// `workflows.validate` and `workflows.put` input: the IR document as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDocument {
    /// The workflow document (O-02).
    pub workflow: Value,
}

/// A validator finding (O-02 section 10). Never echoes a configuration value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Error or warning.
    pub severity: Severity,
    /// Stable code, `ir.<reason>`.
    pub code: String,
    /// JSON Pointer to the member concerned.
    pub path: String,
    /// Explanation for people.
    pub message: String,
}

impl From<&ogun_ir::Diagnostic> for Diagnostic {
    fn from(d: &ogun_ir::Diagnostic) -> Self {
        Self {
            severity: d.severity,
            code: d.code.to_owned(),
            path: d.path.clone(),
            message: d.message.clone(),
        }
    }
}

/// `workflows.validate` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateResult {
    /// No error diagnostics.
    pub ok: bool,
    /// Workflow hash, when the document is valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_hash: Option<String>,
    /// Diagnostics; never echo values.
    pub diagnostics: Vec<Diagnostic>,
}

/// `workflows.put` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PutWorkflowParams {
    /// The workflow document (O-02).
    pub workflow: Value,
    /// Idempotency key.
    pub idempotency_key: String,
    /// Code bundles the workflow references and the daemon does not have yet: BLAKE3 hex to
    /// base64 content. The daemon verifies each against its hash and stores it read-only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub code: BTreeMap<String, String>,
}

/// Why a stored workflow version cannot run yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// The approval.
    pub approval_id: ApprovalId,
    /// Its kind.
    pub kind: ApprovalKind,
}

/// `workflows.put` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutWorkflowResult {
    /// The workflow, all versions.
    pub workflow_id: WorkflowId,
    /// Name.
    pub name: String,
    /// Version stored.
    pub version: u64,
    /// Its hash.
    pub workflow_hash: String,
    /// `false` when this exact version was already stored.
    pub created: bool,
    /// Warnings (errors fail the call with `ir.invalid`).
    pub diagnostics: Vec<Diagnostic>,
    /// Approvals this version needs before it can run (code changes, egress, budgets over policy).
    pub approvals: Vec<PendingApproval>,
}

/// A workflow in a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSummary {
    /// Id.
    pub workflow_id: WorkflowId,
    /// Name.
    pub name: String,
    /// Latest stored version.
    pub latest_version: u64,
    /// Its hash.
    pub workflow_hash: String,
    /// Whether triggers fire.
    pub enabled: bool,
    /// Every approval the latest version needs is granted.
    pub runnable: bool,
    /// When the latest version was stored.
    pub updated_at: Timestamp,
    /// The workflow's description, as written by its author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untrusted_description: Option<String>,
}

/// `workflows.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListWorkflowsResult {
    /// Workflows, by name.
    pub workflows: Vec<WorkflowSummary>,
}

/// `workflows.get` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetWorkflowParams {
    /// Workflow name.
    pub name: String,
    /// Version; latest when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
}

/// `workflows.get` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetWorkflowResult {
    /// Summary of the workflow.
    pub summary: WorkflowSummary,
    /// The version returned.
    pub version: u64,
    /// Its hash.
    pub workflow_hash: String,
    /// The document in normal form.
    pub workflow: Value,
}

/// `workflows.set_enabled` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEnabledParams {
    /// Workflow name.
    pub name: String,
    /// Fire triggers or not.
    pub enabled: bool,
}

/// Which partitions a run covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PartitionSelector {
    /// The unpartitioned run, or the latest complete partition of a partitioned workflow.
    Latest,
    /// Exactly these keys.
    Keys {
        /// Partition keys.
        keys: Vec<String>,
    },
    /// Every key from `from` to `to`, inclusive (a backfill).
    Range {
        /// First key.
        from: String,
        /// Last key.
        to: String,
        /// Re-run partitions already up to date for this workflow hash.
        #[serde(default)]
        force: bool,
    },
}

/// `runs.start` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRunParams {
    /// Workflow name.
    pub workflow: String,
    /// Version; latest when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// When present, must equal the version's hash (`ir.hash_mismatch` otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_hash: Option<String>,
    /// Partitions.
    pub partitions: PartitionSelector,
    /// Idempotency key.
    pub idempotency_key: String,
}

/// One node instance in a run view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeView {
    /// The instance.
    pub instance_id: NodeInstanceId,
    /// Node id.
    pub node: String,
    /// Partition key, `""` when unpartitioned.
    pub partition: String,
    /// State.
    pub state: NodeState,
    /// Attempts started.
    pub attempt: u32,
    /// Last cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cause: Option<Cause>,
    /// Last change.
    pub updated_at: Timestamp,
}

/// A run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunView {
    /// Run id.
    pub run_id: RunId,
    /// Workflow name.
    pub workflow: String,
    /// Version.
    pub version: u64,
    /// The hash the run is bound to.
    pub workflow_hash: String,
    /// Derived state.
    pub state: RunState,
    /// How many partitions.
    pub partition_count: u64,
    /// When created.
    pub created_at: Timestamp,
    /// Node instances; empty in listings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<NodeView>,
}

/// `runs.get` and `runs.cancel` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRef {
    /// The run.
    pub run_id: RunId,
}

/// `runs.cancel` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRunParams {
    /// The run.
    pub run_id: RunId,
    /// Idempotency key.
    pub idempotency_key: String,
}

/// `runs.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRunsParams {
    /// Only this workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Only runs created before this one (paging, newest first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<RunId>,
    /// At most this many (default 50, max 500).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `runs.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRunsResult {
    /// Runs, newest first.
    pub runs: Vec<RunView>,
    /// Pass as `before` for the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<RunId>,
}

/// `backfills.plan` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanBackfillParams {
    /// Workflow name.
    pub workflow: String,
    /// First key.
    pub from: String,
    /// Last key.
    pub to: String,
}

/// An inclusive run of consecutive partition keys with one state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionRange {
    /// First key.
    pub from: String,
    /// Last key.
    pub to: String,
    /// Keys in the range.
    pub count: u64,
    /// The state of every key in it.
    pub state: PartitionState,
}

/// Per-partition state for one workflow hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionState {
    /// Outputs committed for this hash.
    Done,
    /// A run is working on it.
    Running,
    /// The last attempt failed.
    Failed,
    /// Never run, or run only under another hash.
    Missing,
}

/// `backfills.plan` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanBackfillResult {
    /// The hash the plan is for.
    pub workflow_hash: String,
    /// Keys that would run.
    pub to_run: u64,
    /// Keys already up to date.
    pub up_to_date: u64,
    /// The range, compressed.
    pub ranges: Vec<PartitionRange>,
}

/// `partitions.status` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionStatusParams {
    /// Workflow name.
    pub workflow: String,
    /// Version; latest when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// First key; the earliest known when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Last key; the latest complete when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// `partitions.status` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionStatusResult {
    /// The hash.
    pub workflow_hash: String,
    /// Range-compressed states.
    pub ranges: Vec<PartitionRange>,
}

/// `events.since` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSinceParams {
    /// Return events with a sequence number above this.
    pub after: u64,
    /// Only this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// At most this many (default 100, max 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Wait up to this long for the first event (max 30 000).
    #[serde(default)]
    pub wait_ms: u64,
}

/// One event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventView {
    /// Sequence number, gap-free and increasing.
    pub seq: u64,
    /// When.
    pub at: Timestamp,
    /// The run.
    pub run_id: RunId,
    /// The node instance, when the event is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<NodeInstanceId>,
    /// The store's event kind and fields.
    pub kind: Value,
}

/// `events.since` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSinceResult {
    /// Events in order.
    pub events: Vec<EventView>,
    /// Pass as `after` next.
    pub last_seq: u64,
}

/// What an approval is for (O-07 section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    /// A workflow version references code (by hash) not approved before for this workflow.
    CodeChange,
    /// A workflow version reaches a network origin not approved before.
    Egress,
    /// A run would exceed the workflow's budget or the daemon's spend policy.
    Budget,
    /// A node asks to publish outside the workspace.
    Publish,
    /// A policy exception the daemon computed.
    Policy,
}

/// An approval's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    /// Waiting for a person.
    Pending,
    /// Approved.
    Approved,
    /// Denied.
    Denied,
    /// Expired without a decision (a denial).
    Expired,
}

/// The channel that carried a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The Ogun CLI on its controlling terminal.
    Terminal,
    /// Ogun Studio on the desktop.
    Studio,
    /// Studio-web through `ogun-server`, with a WebAuthn assertion.
    Webauthn,
}

/// One fact about an approval, computed by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    /// Label, e.g. `code`, `origin`, `budget.max_cost_usd_micro`.
    pub label: String,
    /// Value as text. Never a secret; URLs are redacted.
    pub value: String,
}

/// An approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalView {
    /// Id.
    pub approval_id: ApprovalId,
    /// Kind.
    pub kind: ApprovalKind,
    /// State.
    pub state: ApprovalState,
    /// Workflow name.
    pub workflow: String,
    /// The workflow hash the decision binds to.
    pub workflow_hash: String,
    /// The run, for run-scoped approvals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// The node, for node-scoped approvals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Daemon-computed facts, the only thing a person decides on.
    pub facts: Vec<Fact>,
    /// BLAKE3 over the JCS form of the facts and bindings; a decision must name it.
    pub request_hash: String,
    /// Who asked (credential label and kind).
    pub requested_by: String,
    /// When requested.
    pub created_at: Timestamp,
    /// When it auto-denies.
    pub expires_at: Timestamp,
    /// When decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<Timestamp>,
    /// The channel of the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_via: Option<Channel>,
    /// Who decided (credential label, or the server user id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
}

/// `approvals.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListApprovalsParams {
    /// Only this state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ApprovalState>,
}

/// `approvals.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListApprovalsResult {
    /// Approvals, newest first.
    pub approvals: Vec<ApprovalView>,
}

/// `approvals.get` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRef {
    /// The approval.
    pub approval_id: ApprovalId,
}

/// A decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Approve.
    Approve,
    /// Deny.
    Deny,
}

/// Evidence for a decision. The daemon checks it; a client's word is never evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum Evidence {
    /// The daemon checks the connection's peer: a pinned `ogun` binary with a controlling terminal.
    Terminal,
    /// The daemon checks the connection's peer: the pinned Studio binary.
    Studio,
    /// A WebAuthn assertion whose challenge is `BLAKE3(approval_id || request_hash || decision)`, from an approver
    /// key a person confirmed on the host terminal. Base64url, no padding.
    Webauthn {
        /// Approver key id (the WebAuthn credential id).
        credential_id: String,
        /// `clientDataJSON`.
        client_data_json: String,
        /// `authenticatorData`.
        authenticator_data: String,
        /// The ES256 signature, DER.
        signature: String,
        /// The server user the bridge acts for.
        user: String,
    },
}

/// `approvals.decide` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecideParams {
    /// The approval.
    pub approval_id: ApprovalId,
    /// The decision.
    pub decision: Decision,
    /// Must equal the approval's `request_hash`.
    pub request_hash: String,
    /// Channel evidence.
    pub evidence: Evidence,
}

/// An artifact a run produced (Zeus 13 store; identity is the BLAKE3 digest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactView {
    /// Store-local id (`art_`).
    pub artifact_id: String,
    /// Content digest.
    pub blake3: String,
    /// Size.
    pub size_bytes: u64,
    /// Media type.
    pub media_type: String,
    /// Producing run.
    pub run_id: RunId,
    /// Producing node.
    pub node: String,
    /// Partition key.
    pub partition: String,
    /// Output port name.
    pub output: String,
    /// When committed.
    pub committed_at: Timestamp,
}

/// `artifacts.list` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListArtifactsParams {
    /// The run.
    pub run_id: RunId,
}

/// `artifacts.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListArtifactsResult {
    /// Artifacts.
    pub artifacts: Vec<ArtifactView>,
}

/// `artifacts.get` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetArtifactParams {
    /// Content digest.
    pub blake3: String,
    /// Return the bytes inline (base64) when at most this many; default 0.
    #[serde(default)]
    pub inline_max_bytes: u64,
}

/// `artifacts.get` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetArtifactResult {
    /// Metadata.
    pub artifact: ArtifactView,
    /// Local path, readable by the same user.
    pub path: String,
    /// Bytes, base64, when requested and small enough.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
}

/// `lineage.get` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageParams {
    /// A run, or ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// ... an artifact digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
}

/// `lineage.get` result: OpenLineage-compatible run events (O-05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineageResult {
    /// OpenLineage `RunEvent`s.
    pub events: Vec<Value>,
}

/// One record pushed into a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRecord {
    /// Event time, Unix ms.
    pub event_time_ms: i64,
    /// Partition key within the stream (a device or site id).
    pub key: String,
    /// The record as decoded. Data, not IR: floats are allowed here (O-09).
    pub payload: Value,
}

/// `streams.push` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushParams {
    /// Workflow name.
    pub workflow: String,
    /// Stream source node id.
    pub node: String,
    /// The producer's sequence number of the first record; the daemon drops records it has already accepted.
    pub first_seq: u64,
    /// Records, at most 10 000 per call.
    pub records: Vec<StreamRecord>,
}

/// `streams.push` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushResult {
    /// The producer may forget records up to this sequence number: they are durable in the daemon.
    pub accepted_through: u64,
    /// Records dropped by the stream's `when_full` policy.
    pub dropped: u64,
    /// Records past their window's lateness, routed to `late`.
    pub late: u64,
    /// When the buffer is full under `block`, retry after this long; 0 otherwise.
    pub retry_after_ms: u64,
}

/// A stream source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamView {
    /// Workflow name.
    pub workflow: String,
    /// Node id.
    pub node: String,
    /// Operator, e.g. `stream.mqtt`.
    pub op: String,
    /// Records buffered, not yet in a sealed window.
    pub buffered: u64,
    /// Records dropped since start.
    pub dropped: u64,
    /// Low watermark across keys, Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_ms: Option<i64>,
    /// Windows sealed since start.
    pub windows_sealed: u64,
    /// Windows open now.
    pub windows_open: u64,
}

/// `streams.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListStreamsResult {
    /// Streams.
    pub streams: Vec<StreamView>,
}

/// `streams.status` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRef {
    /// Workflow name.
    pub workflow: String,
    /// Node id.
    pub node: String,
}

/// `credentials.add` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddCredentialParams {
    /// Label, `[a-z0-9_-]{1,64}`, unique among live credentials.
    pub name: String,
    /// Kind.
    pub kind: ClientKind,
}

/// `credentials.add` result. The daemon writes the token to `credentials/<name>.token` (`0600`); it never
/// returns it over the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddCredentialResult {
    /// The new credential.
    pub credential: CredentialView,
    /// Where the token file is.
    pub token_file: String,
}

/// A credential (never its token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialView {
    /// Id.
    pub credential_id: CredentialId,
    /// Kind.
    pub kind: ClientKind,
    /// Label.
    pub name: String,
    /// When created.
    pub created_at: Timestamp,
    /// Revoked.
    pub revoked: bool,
}

/// `credentials.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCredentialsResult {
    /// Credentials.
    pub credentials: Vec<CredentialView>,
}

/// `credentials.revoke` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeCredentialParams {
    /// The credential.
    pub credential_id: CredentialId,
}

/// `secrets.put` parameters. The value is zeroized by the daemon after it is written.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PutSecretParams {
    /// Name, as used in `{"secret_ref": "<name>"}`.
    pub name: String,
    /// The value. Only a Human on a terminal may send this.
    pub value: String,
}

impl std::fmt::Debug for PutSecretParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PutSecretParams")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// `secrets.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSecretsResult {
    /// Names only.
    pub names: Vec<String>,
}

/// `secrets.delete` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    /// Name.
    pub name: String,
}

/// An approver key: a WebAuthn ES256 public key that may sign decisions relayed by a bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproverView {
    /// WebAuthn credential id, base64url.
    pub credential_id: String,
    /// The server user.
    pub user: String,
    /// Relying party id the assertion must name.
    pub rp_id: String,
    /// BLAKE3 of the public key, shown to the person who confirms it.
    pub fingerprint: String,
    /// Confirmed on the host terminal.
    pub confirmed: bool,
    /// When proposed.
    pub created_at: Timestamp,
}

/// `approvers.propose` parameters (bridge).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposeApproverParams {
    /// WebAuthn credential id, base64url.
    pub credential_id: String,
    /// Uncompressed SEC1 P-256 public key, base64url.
    pub public_key: String,
    /// The server user.
    pub user: String,
    /// Relying party id.
    pub rp_id: String,
}

/// `approvers.confirm` and `approvers.revoke` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproverRef {
    /// WebAuthn credential id.
    pub credential_id: String,
    /// For `approvers.confirm`: the fingerprint the person read on the server page; must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// `approvers.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListApproversResult {
    /// Keys.
    pub approvers: Vec<ApproverView>,
}

/// An enrolled edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeView {
    /// Edge id.
    pub edge_id: EdgeId,
    /// Label.
    pub name: String,
    /// Ed25519 public key, base64url.
    pub public_key: String,
    /// When enrolled.
    pub enrolled_at: Timestamp,
    /// Last successful sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<Timestamp>,
}

/// `edges.enroll` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollEdgeParams {
    /// Label.
    pub name: String,
    /// Ed25519 public key, base64url.
    pub public_key: String,
}

/// `edges.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEdgesResult {
    /// Edges.
    pub edges: Vec<EdgeView>,
}

/// `sync.offer` parameters: an edge offers sealed partitions (O-11 section 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOfferParams {
    /// The edge.
    pub edge_id: EdgeId,
    /// The offer, as `ogun_edge::sync::Offer` JSON.
    pub offer: Value,
    /// Ed25519 signature over the JCS form of `offer`, base64url.
    pub signature: String,
}

/// `sync.offer` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOfferResult {
    /// What the hub wants, as `ogun_edge::sync::Want` JSON.
    pub want: Value,
}

/// `sync.chunk` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncChunkParams {
    /// The edge.
    pub edge_id: EdgeId,
    /// Partition key.
    pub partition: String,
    /// Sealed object digest.
    pub blake3: String,
    /// Chunk index.
    pub index: u32,
    /// Chunks in total.
    pub total: u32,
    /// Bytes, base64 (at most 768 KiB per chunk on this transport).
    pub data_base64: String,
}

/// `sync.chunk` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncChunkResult {
    /// The object is complete and verified.
    pub complete: bool,
}

/// An empty result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}
