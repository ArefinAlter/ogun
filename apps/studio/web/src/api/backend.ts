// One data interface, two implementations: `tauri` (the desktop and mobile shells, through the closed command
// set) and `http` (Studio-web, through ogun-server's REST routes). Views only ever see this interface.

import type {
  ApprovalView,
  CredentialView,
  Decision,
  EdgeView,
  EventView,
  GetArtifactResult,
  GetWorkflowResult,
  LadderResult,
  LineageResult,
  ListApprovalsResult,
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
  StartRunParams,
  StatusResult,
  StreamView,
  ValidateResult,
  WorkflowDoc,
  ApprovalState,
} from './types'

/** Where Studio runs and what this connection may do. */
export interface ShellInfo {
  backend: 'tauri' | 'http'
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
  workspaceId?: string
  workspaces?: { workspace_id: string; name: string; role: string; approver: boolean }[]
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

export interface Backend {
  readonly kind: 'tauri' | 'http'
  info(): Promise<ShellInfo>
  /** Retires this HTTP connection and closes its streams before changing workspace. */
  selectWorkspace?(workspaceId: string): Backend

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
