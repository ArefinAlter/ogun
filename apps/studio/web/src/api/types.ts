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
  /** Reported by the scripted demo only: the month's model spend against its cap. */
  spend?: { month_usd_micro: number; cap_usd_micro: number; entries: number }
}

// ---------------------------------------------------------------- connections, schedules, storage, archive
// Settings pages beyond the closed O-15 set. A daemon that does not offer them leaves the matching `Backend`
// member undefined and Studio says so; the scripted demo implements all of them (see O-16, Proposed).

export type ConnectionState = 'untested' | 'healthy' | 'failing'

export type ConnectionValue = string | number | boolean

/** A named, configured use of a connector: plain settings, and the NAMES of secrets (never their values). */
export interface ConnectionView {
  connection_id: string
  name: string
  connector: string
  auth: string
  settings: Record<string, ConnectionValue>
  /** Auth field name -> the name of the secret that holds it (`ogun secrets set <name>`). */
  secret_refs: Record<string, string>
  state: ConnectionState
  last_test_at?: Timestamp
  last_test_ms?: number
  last_error?: string
  created_at: Timestamp
  used_by: string[]
}

export interface ConnectionInput {
  name: string
  connector: string
  auth: string
  settings: Record<string, ConnectionValue>
  secret_refs: Record<string, string>
}

export interface ConnectionCheck {
  name: string
  ok: boolean
  detail: string
}

export interface ConnectionTestResult {
  ok: boolean
  elapsed_ms: number
  simulated: boolean
  checks: ConnectionCheck[]
}

export interface ScheduleView {
  workflow: string
  version: number
  trigger_kind: string
  /** The cron expression for `cron` triggers. */
  cron?: string
  /** For stream triggers: the source id. */
  source?: string
  enabled: boolean
  runnable: boolean
  last_run_at?: Timestamp
  last_run_state?: RunState
}

export interface StorageInfo {
  root: string
  artifacts: { count: number; bytes: number }
  runs: { count: number }
  caches: { bytes: number }
  limit_bytes: number
  retention_days: number
  by_workflow: { workflow: string; artifacts: number; bytes: number }[]
}

export type ArchiveKind = 'workflow_version' | 'run'

export interface ArchiveItem {
  id: string
  kind: ArchiveKind
  name: string
  detail: string
  archived_at: Timestamp
  reason: string
}

// ---------------------------------------------------------------- runtime defaults, environments, sources, evidence, storage rules
// More settings a daemon may offer (O-16, Proposed). Each is an optional `Backend` member; a daemon without it is
// shown the matching page read-only, with the honest "this daemon does not report this" notice.

export interface RuntimeSettings {
  /** Node instances running at once, per run. */
  concurrency: number
  /** Default retries per node (`policy.retries`). */
  retries: number
  /** Default wall-clock budget in minutes; 0 means none. */
  wall_minutes: number
  /** Default bytes budget in GiB; 0 means none. */
  bytes_gib: number
  /** How long an approval request waits before it expires. */
  approval_expiry_hours: number
  /** `off`: no model operator may run. `ladder`: operators may use the provider ladder. */
  model_access: 'off' | 'ladder'
  /** Pause a schedule after this many failed runs in a row; 0 means never. */
  auto_pause_after_failures: number
  /** Archive finished runs after this many days; 0 means never. */
  auto_archive_days: number
  /** Never auto-archive a run with a failed or waiting partition. */
  keep_attention_runs: boolean
  /** Start runs that were in flight when the daemon last stopped. */
  resume_on_start: boolean
}

export interface RuntimeSettingsView {
  values: RuntimeSettings
  /** The workflow this view is for, or null for the daemon-wide defaults. */
  scope_workflow: string | null
  /** Members this workflow overrides (always empty for the daemon-wide view). */
  overridden: (keyof RuntimeSettings)[]
}

export type EnvironmentKind = 'local' | 'hub' | 'edge'

export interface EnvironmentView {
  id: string
  name: string
  kind: EnvironmentKind
  connected: boolean
  version?: string
  endpoint?: string
  detail?: string
  last_seen_at?: Timestamp
  error?: string
}

export interface BridgeView {
  id: 'zeus' | 'ingest' | 'sync' | 'server' | 'remote_approvals'
  name: string
  state: 'running' | 'stopped' | 'not_configured' | 'locked'
  detail: string
}

export interface EnvironmentsResult {
  environments: EnvironmentView[]
  bridges: BridgeView[]
  scheduler_enabled: boolean
}

export type SourceKind = 'folder' | 'git'

export interface SourceRepo {
  id: string
  name: string
  kind: SourceKind
  location: string
  branch?: string
  watch: boolean
  import_mode: 'store' | 'store_validate'
  workflows: number
  last_sync_at?: Timestamp
  last_result?: string
}

export interface SourceProvider {
  id: string
  name: string
  region: 'global' | 'cn'
  state: 'available' | 'not_authenticated' | 'not_installed'
  detail: string
  enabled: boolean
}

export interface SourcesResult {
  repos: SourceRepo[]
  providers: SourceProvider[]
  auto_pull: boolean
}

export interface EvidenceSettings {
  enabled: boolean
  format: 'json' | 'html'
  include_lineage: boolean
  include_previews: boolean
}

export interface StorageRules {
  store_location: string
  limit_gib: number
  retention_days: number
  delete_failed_after_days: number
  delete_cancelled_after_days: number
  delete_partials_after_days: number
  delete_logs_after_days: number
  clear_caches_after_days: number
  keep_lineage_referenced: boolean
}

export interface CleanupPlan {
  artifacts: number
  bytes: number
  caches_bytes: number
  logs: number
  lines: { rule: string; artifacts: number; bytes: number }[]
}

export interface LadderProvider {
  name: string
  kind: 'anthropic' | 'openai' | 'openai_compatible' | 'ollama' | 'google'
  enabled: boolean
  base_url: string
  model: string
  api_key_secret?: string
  input_per_mtok_usd_micro: number
  output_per_mtok_usd_micro: number
  max_output_tokens: number
  timeout_ms: number
  capabilities: ('chat' | 'json_schema' | 'embeddings')[]
  allow_insecure_http: boolean
}

// ---------------------------------------------------------------- agent threads (O-17, proposed)
//
// A thread is a conversation between a person and an agent that works on the daemon through a closed set of tools.
// The agent's words are untrusted text. Everything with consequences is a card made from the daemon's own answers
// (a validation result, a plan, an approval request), and every action on a card is a button that a person presses.
// The agent has no tool that decides an approval, reads a secret value or runs a shell.

/** What the agent may do in a thread: read, also store workflow versions, or also start runs. */
export type AgentAccess = 'read' | 'author' | 'operate'
export type AgentThreadState = 'idle' | 'running' | 'waiting'

export interface AgentModel {
  id: string
  label: string
  provider: string
  /** False for an entry the daemon cannot call yet; `note` says why. */
  available: boolean
  note?: string
}

export interface AgentToolInfo {
  name: string
  /** The least access that may call it. */
  access: AgentAccess
  summary: string
}

export interface AgentSettings {
  /** Standing instructions for every thread, like an AGENTS.md. Untrusted by the daemon: never holds a secret. */
  instructions: string
  default_access: AgentAccess
  default_model: string
  /** Model spend allowed per thread, in US dollars; 0 means no model call. */
  spend_cap_usd: number
  /** The tools the agent may call at all (an allow-list over the tool table). */
  tools_enabled: string[]
  retention_days: number
  /** Global offline switch: while on, no model call is made at all. */
  offline: boolean
  /** The ladder entries the person allowed to receive prompts. Empty by default: nothing is sent anywhere until a person ticks one. */
  consented_providers: string[]
  /** Model spend allowed per UTC day, in US dollars; 0 means no model call. */
  day_cap_usd: number
}

/** Exactly what the next model request would carry, redacted: shown before anything is sent. */
export interface AgentPreview {
  provider: string
  model: string
  host: string
  system: string
  messages: { role: string; content: string }[]
  tools: string[]
}

export interface AgentThreadSummary {
  thread_id: string
  title: string
  state: AgentThreadState
  access: AgentAccess
  model: string
  created_at: string
  updated_at: string
  item_count: number
  /** Model spend of this thread so far, in micro US dollars, when the daemon counts it (the real engine does). */
  spend_usd_micro?: number
  input_tokens?: number
  output_tokens?: number
}

/** Something the person pointed the agent at with `@`. */
export interface Mention {
  kind: 'workflow' | 'run' | 'connector' | 'approval'
  id: string
  label: string
}

export interface AgentApprovalRef {
  approval_id: string
  kind: string
  workflow: string
  node?: string
  state: string
  requested_by: string
}

export type AgentCard =
  | { type: 'plan'; title: string; steps: { text: string; state: 'todo' | 'doing' | 'done' }[] }
  | { type: 'proposal'; workflow: WorkflowDoc; hash?: string; valid: boolean; diagnostics: Diagnostic[]; needs: string[]; stored?: { version: number } }
  | { type: 'backfill'; workflow: string; from: string; to: string; to_run: number; up_to_date: number; started?: { run_id: string } }
  | { type: 'approvals'; approvals: AgentApprovalRef[] }
  | { type: 'run'; run_id: string; workflow: string }
  | { type: 'table'; title: string; columns: string[]; rows: string[][]; source?: string }
  | { type: 'connector'; connector_id: string; why: string }
  | { type: 'retry'; run_id: string; workflow: string; node: string; cause: string; started?: { run_id: string } }

interface AgentItemBase {
  id: string
  at: string
}

export type AgentItem =
  | (AgentItemBase & { kind: 'user'; text: string; mentions: Mention[] })
  | (AgentItemBase & { kind: 'assistant'; text: string; streaming: boolean })
  | (AgentItemBase & {
      kind: 'tool'
      tool: string
      summary: string
      input?: unknown
      output?: unknown
      state: 'running' | 'ok' | 'error' | 'denied'
      ms?: number
    })
  | (AgentItemBase & { kind: 'card'; card: AgentCard })
  | (AgentItemBase & { kind: 'notice'; tone: 'info' | 'warn'; text: string })

export interface AgentThread extends AgentThreadSummary {
  items: AgentItem[]
  mentions: Mention[]
}

/** What a person can press on a card. The daemon does it with the person's own role, never the agent's. */
export type AgentAction = 'store' | 'start_backfill' | 'retry'

// ---------------------------------------------------------------- the desktop's own engine (ADR 0015)

export type EngineStateName = 'starting' | 'running' | 'restarting' | 'failed' | 'stopped' | 'external'

/** What the shell's supervisor reports about the `ogund` it runs for this Studio. */
export interface EngineInfo {
  state: EngineStateName
  pid?: number
  version?: string
  /** The restart attempt, while `restarting`. */
  attempt?: number
  /** Why the last run ended, while `restarting` or `failed`. */
  error?: string
  restarts: number
  log_path: string
  state_dir: string
}

export interface EngineReport extends EngineInfo {
  /** The end of the daemon's log, present while it is not running. */
  log_tail: string[]
}

/** What `sandbox.status` says about running code operators (O-03 section 4). */
export interface SandboxStatus {
  /** Whether child operators can run at all on this machine. */
  available: boolean
  /** `bwrap`, `sandbox-exec`, `appcontainer` or `none`. */
  backend: string
  /** One plain sentence. */
  summary: string
  /** What the sandbox does isolate, one fact per line. */
  isolates?: string[]
  /** What it does not isolate, one fact per line. */
  does_not_isolate?: string[]
}

export interface ShellPrefs {
  /** Closing the window hides Studio to the tray; the engine keeps running and its schedules keep firing. */
  keep_in_tray: boolean
}

export interface SampleInfo {
  id: string
  name: string
  description: string
  workflows: string[]
  /** How many of the workflows the engine already holds. */
  stored: number
}

export interface SampleImportResult {
  project: string
  workflows: { name: string; version: number; workflow_hash: string; created: boolean; approvals: number }[]
}

/** A hub this Studio can use besides its own engine. */
export interface HubEnvironment {
  id: string
  name: string
  url: string
  auth: 'token' | 'oidc'
  /** For `oidc`: whether tokens are held and still refresh. */
  signed_in: boolean
  /** The signed-in user's name, when there is one. */
  user?: string
  /** This is the environment Studio shows now. */
  active: boolean
}
