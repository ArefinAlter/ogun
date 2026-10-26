// The Tauri backend: the closed command set of `apps/studio/src-tauri/src/commands.rs`. The Rust process holds
// the daemon credential or the hub token; this side sees typed results only.

import type { SvcTransport } from './svc'
import { invoke } from '@tauri-apps/api/core'

import { idempotencyKey } from '../lib/format'
import type { Backend, EngineApi, EventSubscription, HubsApi, PreparedDecision, ShellInfo } from './backend'
import { asOgunError } from './backend'
import { toDaemonCall } from './localMap'
import type {
  ApprovalState,
  ApprovalView,
  Decision,
  EngineInfo,
  EngineReport,
  EventsSinceResult,
  HubEnvironment,
  OgunError,
  SampleImportResult,
  SampleInfo,
  SandboxStatus,
  ShellPrefs,
} from './types'

interface RustInfo {
  app_version: string
  platform: string
  mobile: boolean
  link: 'none' | 'local' | 'hub'
  hub_url?: string
  profile?: string
  connected: boolean
  error?: OgunError
  kind?: string
  can_write: boolean
  can_decide: boolean
  can_read_settings: boolean
  gateway: boolean
  engine?: EngineInfo
}

interface RustPrepared {
  confirmation_id: string
  decision: Decision
  approval: ApprovalView
  arm_ms: number
  expires_ms: number
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args)
  } catch (e) {
    throw asOgunError(e)
  }
}

function mapInfo(i: RustInfo): ShellInfo {
  return {
    backend: 'tauri',
    appVersion: i.app_version,
    platform: i.platform,
    mobile: i.mobile,
    link: i.link,
    ...(i.hub_url ? { hubUrl: i.hub_url } : {}),
    ...(i.profile ? { profile: i.profile } : {}),
    connected: i.connected,
    ...(i.error ? { error: i.error } : {}),
    canWrite: i.link === 'local' && i.can_write,
    decideChannel: i.link === 'local' && i.can_decide && !i.mobile ? 'studio' : null,
    canReadSettings: i.can_read_settings,
    gateway: i.gateway,
    ...(i.engine ? { engine: i.engine } : {}),
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

export function tauriBackend(): Backend {
  return {
    kind: 'tauri',
    info: async () => mapInfo(await call<RustInfo>('studio_info')),
    linkLocal: async () => mapInfo(await call<RustInfo>('link_local')),
    linkHub: async (url, token, caPem, remember) =>
      mapInfo(await call<RustInfo>('link_hub', { url, token, ca_pem: caPem, remember })),
    linkForget: async () => mapInfo(await call<RustInfo>('link_forget')),

    status: () => call('daemon_status'),
    workflowsList: () => call('workflows_list'),
    workflowsGet: (name, version) =>
      call('workflows_get', { params: { name, ...(version !== undefined ? { version } : {}) } }),
    workflowsValidate: (workflow) => call('workflows_validate', { params: { workflow } }),
    workflowsPut: (workflow, idempotency_key) =>
      call('workflows_put', { params: { workflow, idempotency_key } }),

    runsList: (params) => call('runs_list', { params }),
    runsGet: (run_id) => call('runs_get', { params: { run_id } }),
    runsStart: (params) => call('runs_start', { params }),
    runsCancel: (run_id, idempotency_key) => call('runs_cancel', { params: { run_id, idempotency_key } }),

    partitionsStatus: (params) => call('partitions_status', { params }),
    backfillsPlan: (workflow, from, to) => call('backfills_plan', { params: { workflow, from, to } }),

    subscribe(opts, onEvent, onError): EventSubscription {
      let closed = false
      ;(async () => {
        let after: number = opts.after ?? -1
        if (after < 0) {
          try {
            after = (await call<{ last_event_seq: number }>('daemon_status')).last_event_seq
          } catch (e) {
            onError?.(asOgunError(e))
            after = 0
          }
        }
        let backoff = 1000
        while (!closed) {
          try {
            const r: EventsSinceResult = await call<EventsSinceResult>('events_since', {
              params: {
                after,
                wait_ms: 25000,
                limit: 500,
                ...(opts.runId ? { run_id: opts.runId } : {}),
              },
            })
            backoff = 1000
            for (const e of r.events) {
              if (closed) return
              if (e.seq > after) {
                after = e.seq
                onEvent(e)
              }
            }
            after = Math.max(after, r.last_seq)
          } catch (e) {
            if (closed) return
            onError?.(asOgunError(e))
            await sleep(backoff)
            backoff = Math.min(backoff * 2, 30000)
          }
        }
      })()
      return {
        close() {
          closed = true
        },
      }
    },

    approvalsList: (state?: ApprovalState) => call('approvals_list', { params: state ? { state } : {} }),
    approvalsGet: (approval_id) => call('approvals_get', { params: { approval_id } }),
    async prepareDecision(approvalId, decision): Promise<PreparedDecision> {
      const p = await call<RustPrepared>('approvals_prepare', { approval_id: approvalId, decision })
      let used = false
      return {
        decision: p.decision,
        approval: p.approval,
        armMs: p.arm_ms,
        async confirm() {
          if (used) throw asOgunError({ code: 'api.invalid_params', message: 'this review was already used' })
          used = true
          return call<ApprovalView>('approvals_confirm', { confirmation_id: p.confirmation_id })
        },
        cancel() {
          if (!used) {
            used = true
            void call('approvals_discard', { confirmation_id: p.confirmation_id }).catch(() => undefined)
          }
        },
      }
    },

    artifactsList: (run_id) => call('artifacts_list', { params: { run_id } }),
    artifactsGet: (blake3, inline_max_bytes) => call('artifacts_get', { params: { blake3, inline_max_bytes } }),
    lineageGet: (q) => call('lineage_get', { params: q }),

    streamsList: () => call('streams_list'),
    streamsStatus: (workflow, node) => call('streams_status', { params: { workflow, node } }),
    edgesList: () => call('edges_list'),

    credentialsList: () => call('credentials_list'),
    secretsList: () => call('secrets_list'),
    llmLadder: () => call('llm_ladder'),
  }
}

/** The service transport of the engine Studio runs for itself: the service's names, translated to the daemon's
 *  (`localMap.ts`) and relayed by the shell's `engine_call`, which only forwards a closed list of methods. */
export function localSvcTransport(): SvcTransport {
  return {
    call: (method, params = {}) => {
      const c = toDaemonCall(method, params, idempotencyKey)
      return call('engine_call', { method: c.method, params: c.params })
    },
  }
}

/** Studio's own engine, the shell's preferences, the bundled samples and secret entry. */
export function localEngineApi(): EngineApi {
  return {
    report: () => call<EngineReport>('engine_info'),
    restart: () => call<void>('engine_restart'),
    prefs: () => call<ShellPrefs>('shell_prefs_get'),
    setPrefs: (prefs) => call<ShellPrefs>('shell_prefs_set', { prefs }),
    samples: () => call<SampleInfo[]>('samples_list'),
    importSample: (id) => call<SampleImportResult>('sample_import', { id }),
    putSecret: (name, value) => call<void>('secret_put', { name, value }),
    deleteSecret: (name) => call<void>('secret_delete', { name }),
    sandboxStatus: () => call<SandboxStatus>('engine_call', { method: 'sandbox.status', params: {} }),
  }
}

/** Hubs reached with a browser sign-in. The commands arrive with the shell's auth layer; until then each call
 *  fails with the shell's own error, which the Environments page shows. */
export function localHubsApi(): HubsApi {
  return {
    list: () => call<HubEnvironment[]>('hub_environments'),
    signIn: (url, ca_pem) => call<HubEnvironment | null>('hub_signin_start', { url, ca_pem }),
    signOut: (id) => call<void>('hub_signout', { id }),
    use: async (id) => mapInfo(await call<RustInfo>('hub_use', { id })),
  }
}

/** The Android gateway plugin (Kotlin), reached through Tauri's plugin route. */
export const gateway = {
  status: () => call<GatewayStatus>('plugin:ogun-gateway|status'),
  configure: (settings: GatewaySettings) => call<GatewayStatus>('plugin:ogun-gateway|configure', { settings }),
  start: () => call<GatewayStatus>('plugin:ogun-gateway|start'),
  stop: () => call<GatewayStatus>('plugin:ogun-gateway|stop'),
  syncNow: () => call<GatewayStatus>('plugin:ogun-gateway|sync_now'),
}

export interface GatewaySettings {
  name: string
  edge_id?: string
  hub_url?: string
  hub_ca_pem?: string
  mqtt_url?: string
  mqtt_topics: string[]
  mqtt_username?: string
  /** Write-only: kept in the Android Keystore; empty keeps the stored value. */
  mqtt_password?: string
  http_push_enabled: boolean
  http_push_port: number
  /** Write-only bearer token LAN devices must send; empty keeps the stored value. */
  http_push_token?: string
  sync_requires_charging: boolean
  segment_ms: number
  max_log_bytes: number
  when_full: 'block' | 'drop_oldest' | 'drop_newest'
}

export interface GatewayStatus {
  running: boolean
  settings: Omit<GatewaySettings, 'mqtt_password' | 'http_push_token'> & {
    mqtt_password_set: boolean
    http_push_token_set: boolean
  }
  mqtt?: { connected: boolean; last_error?: string; received: number }
  http_push?: { listening: boolean; address?: string; received: number }
  stopped_reason?: string
  core?: {
    name: string
    asset: string
    edge_id?: string
    public_key: string
    fingerprint: string
    log: {
      next_seq: number
      bytes_used: number
      released_through?: number
      dropped_newest: number
      lost_oldest: number
      evicted_released: number
    }
    stored_records: number
    sealed_through?: number
    pending_segments: number
    pending_bytes: number
    acked_segments: number
    acked_bytes: number
    invalid: number
    last_seal_ms?: number
    last_sync?: { at_ms: number; ok: boolean; reason: string }
  }
  core_error?: OgunError
}

/** The hosted pre-alpha service over the hub link (`prealpha_call`, a closed method list in the shell). */
export function tauriSvcTransport(install: string): SvcTransport {
  return { call: (method, params = {}) => call('prealpha_call', { method, params, install }) }
}
