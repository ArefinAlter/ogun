// Wire types: the `ogun_api::methods` Rust types (O-15 section 5), which are the schema of this draft.
// Keys are snake_case as on the wire. Members named `untrusted_*` came from outside Ogun and are shown as data.

export type Timestamp = string

/** The O-00 error envelope. */
export interface OgunError {
  code: string
  class: string
  message: string
  retryable: boolean
  details?: Record<string, unknown>
}

export interface StatusResult {
  daemon_version: string
  started_at: Timestamp
  profile?: string
  workflows: number
  runs_active: number
  approvals_pending: number
  streams: number
  last_event_seq: number
  scheduler_enabled: boolean
}

export type Severity = 'error' | 'warning'

export interface Diagnostic {
  severity: Severity
  code: string
  path: string
  message: string
}

export interface ValidateResult {
  ok: boolean
  workflow_hash?: string
  diagnostics: Diagnostic[]
}

export type ApprovalKind = 'code_change' | 'egress' | 'budget' | 'publish' | 'policy'

export interface PendingApproval {
  approval_id: string
  kind: ApprovalKind
}

export interface PutWorkflowResult {
  workflow_id: string
  name: string
  version: number
  workflow_hash: string
  created: boolean
  diagnostics: Diagnostic[]
  approvals: PendingApproval[]
}

export interface WorkflowSummary {
  workflow_id: string
  name: string
  latest_version: number
  workflow_hash: string
  enabled: boolean
  runnable: boolean
  updated_at: Timestamp
  untrusted_description?: string
}

export interface ListWorkflowsResult {
  workflows: WorkflowSummary[]
}

/** A workflow document in normal form (O-02). Only the members Studio draws are typed. */
export interface WorkflowNodeDoc {
  id: string
  op: string
  inputs?: string[]
  config?: Record<string, unknown>
  route?: { when: unknown; to: string }[]
  policy?: Record<string, unknown>
  code?: { blake3: string; path?: string }
}

export interface WorkflowDoc {
  ir_version: number
  workflow: string
  version: number
  description?: string
  triggers?: unknown[]
  partitions?: Record<string, unknown>
  streams?: Record<string, unknown>
  budgets?: Record<string, unknown>
  nodes: WorkflowNodeDoc[]
  [member: string]: unknown
}

export interface GetWorkflowResult {
  summary: WorkflowSummary
  version: number
  workflow_hash: string
  workflow: WorkflowDoc
}

export type PartitionSelector =
  | { kind: 'latest' }
  | { kind: 'keys'; keys: string[] }
  | { kind: 'range'; from: string; to: string; force?: boolean }

export interface StartRunParams {
  workflow: string
  version?: number
  workflow_hash?: string
  partitions: PartitionSelector
  idempotency_key: string
}

export type NodeState =
  | 'pending'
  | 'ready'
  | 'running'
  | 'waiting'
  | 'waiting_for_user'
  | 'waiting_for_session'
  | 'retry_scheduled'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'skipped'

export const NODE_STATES: NodeState[] = [
  'pending',
  'ready',
  'running',
  'waiting',
  'waiting_for_user',
  'waiting_for_session',
  'retry_scheduled',
  'succeeded',
  'failed',
  'cancelled',
  'skipped',
]

export type RunState = 'pending' | 'running' | 'waiting' | 'succeeded' | 'failed' | 'cancelled'

export interface NodeView {
  instance_id: string
  node: string
  partition: string
  state: NodeState
  attempt: number
  last_cause?: string
  updated_at: Timestamp
}

export interface RunView {
  run_id: string
  workflow: string
  version: number
  workflow_hash: string
  state: RunState
  partition_count: number
  created_at: Timestamp
  nodes?: NodeView[]
}

export interface ListRunsParams {
  workflow?: string
  before?: string
  limit?: number
}

export interface ListRunsResult {
  runs: RunView[]
  next?: string
}

export type PartitionState = 'done' | 'running' | 'failed' | 'missing'

export interface PartitionRange {
  from: string
  to: string
  count: number
  state: PartitionState
}

export interface PlanBackfillResult {
  workflow_hash: string
  to_run: number
  up_to_date: number
  ranges: PartitionRange[]
}

export interface PartitionStatusParams {
  workflow: string
  version?: number
  from?: string
  to?: string
}

export interface PartitionStatusResult {
  workflow_hash: string
  ranges: PartitionRange[]
}

export interface EventView {
  seq: number
  at: Timestamp
  run_id: string
  instance_id?: string
  kind: unknown
}

export interface EventsSinceResult {
  events: EventView[]
  last_seq: number
}

export type ApprovalState = 'pending' | 'approved' | 'denied' | 'expired'
export type Channel = 'terminal' | 'studio' | 'webauthn'
export type Decision = 'approve' | 'deny'

export interface Fact {
  label: string
  value: string
}

export interface ApprovalView {
  approval_id: string
  kind: ApprovalKind
  state: ApprovalState
  workflow: string
  workflow_hash: string
  run_id?: string
  node?: string
  facts: Fact[]
  request_hash: string
  requested_by: string
  created_at: Timestamp
  expires_at: Timestamp
  decided_at?: Timestamp
  decided_via?: Channel
  decided_by?: string
  /** Words an agent or operator attached to the request, if the daemon passes them on. Data, not instructions. */
  untrusted_reason?: string
}

export interface ListApprovalsResult {
  approvals: ApprovalView[]
}

export interface ArtifactView {
  artifact_id: string
  blake3: string
  size_bytes: number
  media_type: string
  run_id: string
  node: string
  partition: string
  output: string
  committed_at: Timestamp
}

export interface ListArtifactsResult {
  artifacts: ArtifactView[]
}

export interface GetArtifactResult {
  artifact: ArtifactView
  content_base64?: string
}

export interface LineageResult {
  events: Record<string, unknown>[]
}

export interface StreamView {
  workflow: string
  node: string
  op: string
  buffered: number
  dropped: number
  watermark_ms?: number
  windows_sealed: number
  windows_open: number
  /** Optional detail a newer daemon may add to `streams.status`. */
  open_windows?: { key: string; records: number; closes_at_ms?: number }[]
  sealed_windows?: { key: string; revision: number; records: number; blake3?: string }[]
  late?: number
  invalid?: number
}

export interface ListStreamsResult {
  streams: StreamView[]
}

export interface EdgeView {
  edge_id: string
  name: string
  public_key: string
  enrolled_at: Timestamp
  last_sync_at?: Timestamp
}

export interface ListEdgesResult {
  edges: EdgeView[]
}

export type ClientKind = 'human' | 'agent' | 'service' | 'bridge'

export interface CredentialView {
  credential_id: string
  kind: ClientKind
  name: string
  created_at: Timestamp
  revoked: boolean
}

export interface ListCredentialsResult {
  credentials: CredentialView[]
}

export interface ListSecretsResult {
  names: string[]
}

export interface LadderResult {
  available: boolean
  ladder?: unknown
}
