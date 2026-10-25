// One data interface, two implementations: `tauri` (the desktop and mobile shells, through the closed command
// set) and `http` (Studio-web, through ogun-server's REST routes). Views only ever see this interface.

import type {
  AgentAccess,
  AgentAction,
  AgentModel,
  AgentSettings,
  AgentThread,
  AgentThreadSummary,
  AgentToolInfo,
  ApprovalView,
  ArchiveItem,
  CleanupPlan,
  ConnectionInput,
  ConnectionTestResult,
  ConnectionView,
  CredentialView,
  Decision,
  EdgeView,
  EnvironmentsResult,
  EvidenceSettings,
  EventView,
  GetArtifactResult,
  GetWorkflowResult,
  LadderProvider,
  LadderResult,
  LineageResult,
  ListApprovalsResult,
  Mention,
  ListArtifactsResult,
  ListRunsParams,
  ListRunsResult,
  ListWorkflowsResult,
  OgunError,
  PartitionStatusParams,
  PartitionStatusResult,
  PlanBackfillResult,
  PutWorkflowResult,
  RunView,
  RuntimeSettings,
  RuntimeSettingsView,
  ScheduleView,
  SourcesResult,
  StartRunParams,
  StatusResult,
  StorageInfo,
  StorageRules,
  StreamView,
  ValidateResult,
  WorkflowDoc,
  ApprovalState,
} from './types'

/** Where Studio runs and what this connection may do. */
export interface ShellInfo {
  backend: 'tauri' | 'http' | 'demo'
  appVersion: string
  /** `linux`, `macos`, `windows`, `android`, `ios` or `web`. */
  platform: string
  mobile: boolean
  /** `local` (ogund on this machine), `hub` (a server, read-only), `server` (Studio-web) or `none`. */
  link: 'local' | 'hub' | 'server' | 'none'
  hubUrl?: string
  profile?: string
  connected: boolean
  error?: OgunError
  canWrite: boolean
  /** Which channel decides approvals here: Studio evidence (desktop) or a passkey (Studio-web); none on mobile. */
  decideChannel: 'studio' | 'webauthn' | null
  canReadSettings: boolean
  gateway: boolean
  /** The daemon runs agent threads (O-17). */
  agent?: boolean
  /** Studio-web: the signed-in user. */
  user?: { id: string; name: string; roles: string[]; approver: boolean }
}

/** A decision under review: the facts as the daemon reported them just now, and a single confirmation. */
export interface PreparedDecision {
  decision: Decision
  approval: ApprovalView
  /** The confirm button arms after this many milliseconds. */
  armMs: number
  /** Sends the decision. Studio-web asks for the passkey here. */
  confirm(): Promise<ApprovalView>
  /** Closes the review without deciding. */
  cancel(): void
}

export interface EventSubscription {
  /** Stops the subscription. */
  close(): void
}

/** Controls of the scripted demo backend (`src/demo`): only present when Studio runs on demo data. */
export interface DemoControls {
  /** Simulates `kill -9 ogund` and a restart: finished partitions stay finished, in-flight attempts re-run. */
  restartDaemon(): void
  /** Delivers late sensor records for a sealed window: a new revision, never a silent gap. */
  lateData(): void
  /** Starts the scenario again from its initial state. */
  reset(): void
  /** Pretends the person ran `ogun secrets set <name>` on a terminal: the name becomes a stored secret. */
  simulateSecret(name: string): void
}

/** Data connections (O-16). Secrets are named here, never held: a value is set on a terminal with `ogun secrets set`. */
export interface ConnectionsApi {
  list(): Promise<{ connections: ConnectionView[] }>
  put(input: ConnectionInput, idempotencyKey: string): Promise<ConnectionView>
  test(name: string): Promise<ConnectionTestResult>
  remove(name: string, idempotencyKey: string): Promise<void>
}


/** Agent threads (O-17, proposed). The O-15 daemon has no such methods; the scripted demo implements them. */
export interface AgentApi {
  models(): Promise<AgentModel[]>
  tools(): Promise<AgentToolInfo[]>
  settings(): Promise<AgentSettings>
  setSettings(patch: Partial<AgentSettings>, idempotencyKey: string): Promise<AgentSettings>
  /** Calls `onChange` now and whenever the list changes. */
  watchThreads(onChange: (threads: AgentThreadSummary[]) => void): EventSubscription
  /** Calls `onChange` now and whenever the thread changes, including every streamed word. */
  watchThread(threadId: string, onChange: (thread: AgentThread | null) => void): EventSubscription
  create(opts: { access: AgentAccess; model: string }): Promise<AgentThread>
  /** Starts a turn; the answer arrives through `watchThread`. */
  send(threadId: string, text: string, mentions: Mention[]): Promise<void>
  stop(threadId: string): Promise<void>
  /** A person pressed a button on a card. */
  act(threadId: string, itemId: string, action: AgentAction, idempotencyKey: string): Promise<void>
  setAccess(threadId: string, access: AgentAccess): Promise<void>
  rename(threadId: string, title: string): Promise<void>
  remove(threadId: string): Promise<void>
}

export interface Backend {
  readonly kind: 'tauri' | 'http' | 'demo'
  /** Present on the demo backend only. */
  readonly demo?: DemoControls
  /** Present when the daemon offers agent threads (O-17). Only the scripted demo does today. */
  readonly agent?: AgentApi
  /** Present when the daemon manages data connections (the demo does; the O-15 daemon does not yet). */
  readonly connections?: ConnectionsApi
  /** Trigger schedules of every stored workflow, with pause and resume where the daemon allows it. */
  schedules?(): Promise<{ schedules: ScheduleView[] }>
  setWorkflowEnabled?(name: string, enabled: boolean, idempotencyKey: string): Promise<void>
  addSchedule?(workflow: string, cron: string, idempotencyKey: string): Promise<{ workflow: string; version: number; approvals: number }>
  /** Daemon-wide run defaults, and per-workflow overrides of them. */
  runtime?: {
    get(workflow?: string): Promise<RuntimeSettingsView>
    set(workflow: string | null, patch: Partial<RuntimeSettings>, idempotencyKey: string): Promise<RuntimeSettingsView>
    reset(workflow: string, field: keyof RuntimeSettings, idempotencyKey: string): Promise<RuntimeSettingsView>
  }
  environments?(): Promise<EnvironmentsResult>
  /** Replaces the provider ladder (llm.json). The O-15 daemon does not offer this; the demo does. */
  setLadder?(providers: LadderProvider[], idempotencyKey: string): Promise<void>
  setSchedulerEnabled?(enabled: boolean, idempotencyKey: string): Promise<void>
  sources?: {
    get(): Promise<SourcesResult>
    setRepo(id: string, patch: { watch?: boolean; import_mode?: 'store' | 'store_validate' }, idempotencyKey: string): Promise<SourcesResult>
    sync(id: string, idempotencyKey: string): Promise<SourcesResult>
    setProvider(id: string, enabled: boolean, idempotencyKey: string): Promise<SourcesResult>
    setAutoPull(on: boolean, idempotencyKey: string): Promise<SourcesResult>
  }
  evidence?: {
    get(): Promise<EvidenceSettings>
    set(patch: Partial<EvidenceSettings>, idempotencyKey: string): Promise<EvidenceSettings>
  }
  storage?: {
    info(): Promise<StorageInfo>
    setRetention(days: number, idempotencyKey: string): Promise<StorageInfo>
    clearCaches(idempotencyKey: string): Promise<StorageInfo>
    rules(): Promise<StorageRules>
    setRules(patch: Partial<StorageRules>, idempotencyKey: string): Promise<StorageRules>
    cleanupPlan(): Promise<CleanupPlan>
    cleanup(idempotencyKey: string): Promise<StorageInfo>
  }
  archive?: {
    list(): Promise<{ items: ArchiveItem[] }>
    restore(id: string, idempotencyKey: string): Promise<void>
  }
  info(): Promise<ShellInfo>

  status(): Promise<StatusResult>
  workflowsList(): Promise<ListWorkflowsResult>
  workflowsGet(name: string, version?: number): Promise<GetWorkflowResult>
  workflowsValidate(workflow: unknown): Promise<ValidateResult>
  workflowsPut(workflow: WorkflowDoc | unknown, idempotencyKey: string): Promise<PutWorkflowResult>

  runsList(params: ListRunsParams): Promise<ListRunsResult>
  runsGet(runId: string): Promise<RunView>
  runsStart(params: StartRunParams): Promise<RunView>
  runsCancel(runId: string, idempotencyKey: string): Promise<RunView>

  partitionsStatus(params: PartitionStatusParams): Promise<PartitionStatusResult>
  backfillsPlan(workflow: string, from: string, to: string): Promise<PlanBackfillResult>

  /** Events after `after` (or after the newest one when absent), optionally for one run. */
  subscribe(
    opts: { after?: number; runId?: string },
    onEvent: (e: EventView) => void,
    onError?: (e: OgunError) => void,
  ): EventSubscription

  approvalsList(state?: ApprovalState): Promise<ListApprovalsResult>
  approvalsGet(approvalId: string): Promise<ApprovalView>
  prepareDecision(approvalId: string, decision: Decision): Promise<PreparedDecision>

  artifactsList(runId: string): Promise<ListArtifactsResult>
  artifactsGet(blake3: string, inlineMaxBytes: number): Promise<GetArtifactResult>
  lineageGet(q: { run_id?: string; blake3?: string }): Promise<LineageResult>

  streamsList(): Promise<{ streams: StreamView[] }>
  streamsStatus(workflow: string, node: string): Promise<StreamView>
  edgesList(): Promise<{ edges: EdgeView[] }>

  credentialsList(): Promise<{ credentials: CredentialView[] }>
  secretsList(): Promise<{ names: string[] }>
  llmLadder(): Promise<LadderResult>

  /** Tauri only: choose the link. */
  linkLocal?(): Promise<ShellInfo>
  linkHub?(url: string, token: string, caPem: string | null, remember: boolean): Promise<ShellInfo>
  linkForget?(): Promise<ShellInfo>
}

/** Errors from any backend are O-00 envelopes; anything else is wrapped as one. */
export function asOgunError(e: unknown): OgunError {
  if (e && typeof e === 'object' && 'code' in e && 'message' in e) {
    const o = e as Partial<OgunError>
    return {
      code: String(o.code),
      class: String(o.class ?? 'internal'),
      message: String(o.message),
      retryable: Boolean(o.retryable),
      ...(o.details ? { details: o.details } : {}),
    }
  }
  const message = e instanceof Error ? e.message : typeof e === 'string' ? e : 'unexpected error'
  return { code: 'internal.bug', class: 'internal', message, retryable: false }
}
