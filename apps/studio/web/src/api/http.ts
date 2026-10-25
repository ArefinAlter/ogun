// The HTTP backend (Studio-web): ogun-server's REST routes under /api/v1, the session cookie set at sign-in,
// the CSRF header on every non-GET request, Server-Sent Events for live updates, and WebAuthn for approvals.
// Bodies and results are exactly the `ogun_api::methods` types.

import type { Backend, EventSubscription, PreparedDecision, ShellInfo } from './backend'
import { asOgunError } from './backend'
import type { ApprovalView, EventView, OgunError, StatusResult } from './types'
import { getAssertion, toRequestOptions, webauthnAvailable } from './webauthn'

const BASE = '/api/v1'
const CSRF_HEADER = 'x-ogun-csrf'
const CSRF_COOKIE = 'ogun_csrf'

let csrfToken: string | null = null

function cookie(name: string): string | null {
  for (const part of document.cookie.split(';')) {
    const [k, ...v] = part.trim().split('=')
    if (k === name) return decodeURIComponent(v.join('='))
  }
  return null
}

function path(...segments: string[]): string {
  return BASE + '/' + segments.map(encodeURIComponent).join('/')
}

function query(params: Record<string, string | number | undefined | null>): string {
  const q = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== '') q.set(k, String(v))
  const s = q.toString()
  return s ? '?' + s : ''
}

function classFor(status: number): string {
  if (status === 401) return 'api.unauthenticated'
  if (status === 403) return 'api.permission_denied'
  if (status === 404) return 'api.method_not_found'
  if (status === 409) return 'api.idempotency_conflict'
  if (status >= 400 && status < 500) return 'api.invalid_params'
  return 'api.unavailable'
}

async function request<T>(method: 'GET' | 'POST', url: string, body?: unknown, workspace?: string): Promise<T> {
  const headers: Record<string, string> = { accept: 'application/json' }
  if (workspace) headers['x-ogun-workspace'] = workspace
  if (method !== 'GET') {
    headers['content-type'] = 'application/json'
    const token = csrfToken ?? cookie(CSRF_COOKIE)
    if (token) headers[CSRF_HEADER] = token
  }
  let res: Response
  try {
    res = await fetch(url, {
      method,
      headers,
      credentials: 'same-origin',
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
    })
  } catch {
    throw asOgunError({ code: 'api.unavailable', class: 'unavailable', message: 'cannot reach the server', retryable: true })
  }
  const text = await res.text()
  let json: unknown = null
  try {
    json = text ? JSON.parse(text) : null
  } catch {
    json = null
  }
  if (!res.ok) {
    if (json && typeof json === 'object' && 'error' in json) throw asOgunError((json as { error: unknown }).error)
    throw asOgunError({ code: classFor(res.status), message: `the server answered HTTP ${res.status}` })
  }
  return json as T
}

interface Membership {
  workspace_id: string
  name: string
  role: string
  approver: boolean
}
interface Me {
  user: { user_id: string; display_name: string; email?: string }
  workspaces: Membership[]
  remote_approvals?: boolean
  csrf_token?: string
}

const ROLE_ORDER = ['viewer', 'editor', 'admin', 'owner']

function atLeast(roles: string[], role: string): boolean {
  const need = ROLE_ORDER.indexOf(role)
  return roles.some((r) => ROLE_ORDER.indexOf(r) >= need)
}

export function httpBackend(selectedWorkspace?: string): Backend {
  let workspace = selectedWorkspace
  let memberships: Membership[] = []
  let retired = false
  const subscriptions = new Set<EventSubscription>()
  const checkActive = () => {
    if (retired) throw asOgunError({ code: 'api.invalid_params', message: 'the workspace changed; reopen this page or review' })
  }
  const get = <T>(url: string) => {
    checkActive()
    if (!workspace && url !== path('me')) return Promise.reject(asOgunError({ code: 'api.permission_denied', message: 'select a workspace first' }))
    return request<T>('GET', url, undefined, workspace)
  }
  const post = <T>(url: string, body: unknown) => {
    checkActive()
    if (!workspace) return Promise.reject(asOgunError({ code: 'api.permission_denied', message: 'select a workspace first' }))
    return request<T>('POST', url, body, workspace)
  }
  return {
    selectWorkspace(id) {
      checkActive()
      if (!memberships.some((m) => m.workspace_id === id)) throw asOgunError({ code: 'api.permission_denied', message: 'no membership in this workspace' })
      retired = true
      for (const subscription of subscriptions) subscription.close()
      return httpBackend(id)
    },
    kind: 'http',
    async info(): Promise<ShellInfo> {
      const base: ShellInfo = {
        backend: 'http',
        appVersion: __APP_VERSION__,
        platform: 'web',
        mobile: false,
        link: 'server',
        connected: false,
        canWrite: false,
        decideChannel: null,
        canReadSettings: false,
        gateway: false,
      }
      try {
        const me = await get<Me>(path('me'))
        if (me.csrf_token) csrfToken = me.csrf_token
        checkActive()
        memberships = me.workspaces ?? []
        workspace ??= memberships[0]?.workspace_id
        const membership = memberships.find((m) => m.workspace_id === workspace)
        if (!membership) throw asOgunError({ code: 'api.permission_denied', message: 'no membership in the selected workspace' })
        const roles = [membership.role]
        const approver = membership.approver === true
        return {
          ...base,
          connected: true,
          workspaceId: workspace,
          workspaces: memberships,
          canWrite: atLeast(roles, 'editor'),
          // The server refuses decisions while `remote_approvals` is off (ADR 0010); hide the buttons then.
          decideChannel: approver && me.remote_approvals === true && webauthnAvailable() ? 'webauthn' : null,
          user: {
            id: me.user.user_id,
            name: me.user.display_name || me.user.email || me.user.user_id,
            roles,
            approver,
          },
        }
      } catch (e) {
        return { ...base, error: asOgunError(e) }
      }
    },

    status: () => get<StatusResult>(path('status')),
    workflowsList: () => get(path('workflows')),
    workflowsGet: (name, version) => get(path('workflows', name) + query({ version })),
    workflowsValidate: (workflow) => post(path('workflows', 'validate'), { workflow }),
    workflowsPut: (workflow, idempotency_key) => post(path('workflows'), { workflow, idempotency_key }),

    runsList: (p) => get(path('runs') + query({ workflow: p.workflow, before: p.before, limit: p.limit })),
    runsGet: (runId) => get(path('runs', runId)),
    runsStart: (p) => {
      const { workflow, ...rest } = p
      return post(path('workflows', workflow, 'runs'), rest)
    },
    runsCancel: (runId, idempotency_key) => post(path('runs', runId, 'cancel'), { idempotency_key }),

    partitionsStatus: (p) =>
      get(path('workflows', p.workflow, 'partitions') + query({ version: p.version, from: p.from, to: p.to })),
    backfillsPlan: (workflow, from, to) => post(path('workflows', workflow, 'backfill-plan'), { from, to }),

    subscribe(opts, onEvent, onError): EventSubscription {
      checkActive()
      if (!workspace) throw asOgunError({ code: 'api.permission_denied', message: 'select a workspace first' })
      const subscribedWorkspace = workspace
      let closed = false
      let source: EventSource | null = null
      let last = opts.after
      const open = async () => {
        if (closed) return
        if (last === undefined) {
          try {
            last = (await get<StatusResult>(path('status'))).last_event_seq
          } catch (e) {
            onError?.(asOgunError(e))
            last = 0
          }
        }
        if (closed) return
        source = new EventSource(path('events') + query({ after: last, run_id: opts.runId, workspace: subscribedWorkspace }), {
          withCredentials: true,
        })
        source.addEventListener('ogun', (msg) => {
          if (closed || retired) return
          try {
            const e = JSON.parse((msg as MessageEvent<string>).data) as EventView
            // The browser reconnects by itself; drop anything already seen.
            if (last === undefined || e.seq > last) {
              last = e.seq
              onEvent(e)
            }
          } catch {
            // A frame that is not an event view is ignored.
          }
        })
        source.onerror = () => {
          if (source && source.readyState === EventSource.CLOSED && !closed) {
            onError?.({ code: 'api.unavailable', class: 'unavailable', message: 'the event stream closed', retryable: true })
            setTimeout(open, 3000)
          }
        }
      }
      void open()
      const subscription = {
        close() {
          closed = true
          source?.close()
          subscriptions.delete(subscription)
        },
      }
      subscriptions.add(subscription)
      return subscription
    },

    approvalsList: (state) => get(path('approvals') + query({ state })),
    approvalsGet: (id) => get(path('approvals', id)),
    async prepareDecision(approvalId, decision): Promise<PreparedDecision> {
      const approval = await get<ApprovalView>(path('approvals', approvalId))
      if (approval.state !== 'pending') {
        throw asOgunError({ code: 'approval.already_decided', message: 'this approval is no longer pending' })
      }
      let used = false
      return {
        decision,
        approval,
        armMs: 1500,
        async confirm() {
          if (used) throw asOgunError({ code: 'api.invalid_params', message: 'this review was already used' })
          checkActive()
          used = true
          const options = await post<unknown>(path('approvals', approvalId, 'webauthn-options'), { decision })
          checkActive()
          let assertion
          try {
            assertion = await getAssertion(toRequestOptions(options))
          } catch (e) {
            throw asOgunError({
              code: 'approval.channel_required',
              message: e instanceof Error ? `passkey: ${e.message}` : 'the passkey was not used',
            })
          }
          checkActive()
          // The decision binds to the request hash shown in the review.
          return post<ApprovalView>(path('approvals', approvalId, 'decide'), {
            decision,
            request_hash: approval.request_hash,
            assertion,
          })
        },
        cancel() {
          used = true
        },
      }
    },

    artifactsList: (runId) => get(path('artifacts') + query({ run_id: runId })),
    async artifactsGet(blake3, inlineMaxBytes) {
      const artifactWorkspace = workspace
      const meta = await get<Record<string, unknown>>(path('artifacts', blake3))
      const result = ('artifact' in meta ? meta : { artifact: meta }) as {
        artifact: { size_bytes: number }
        content_base64?: string
      }
      if (inlineMaxBytes > 0 && result.artifact.size_bytes <= inlineMaxBytes && !result.content_base64) {
        checkActive()
        const res = await fetch(path('artifacts', blake3, 'content'), { credentials: 'same-origin', headers: artifactWorkspace ? { 'x-ogun-workspace': artifactWorkspace } : {} })
        if (res.ok) {
          const buf = new Uint8Array(await res.arrayBuffer())
          if (buf.byteLength <= inlineMaxBytes) {
            let bin = ''
            for (const b of buf) bin += String.fromCharCode(b)
            result.content_base64 = btoa(bin)
          }
        }
      }
      return result as never
    },
    lineageGet: (q) => get(path('lineage') + query(q)),

    streamsList: () => get(path('streams')),
    streamsStatus: (workflow, node) => get(path('streams', workflow, node)),
    edgesList: () => get(path('edges')),

    credentialsList: () => Promise.reject(notHere('local credentials are listed in Studio on the daemon host')),
    secretsList: () => Promise.reject(notHere('secret names are listed in Studio on the daemon host')),
    llmLadder: async () => ({ available: false }),
  }
}

function notHere(message: string): OgunError {
  return { code: 'api.method_not_found', class: 'unsupported', message, retryable: false }
}
