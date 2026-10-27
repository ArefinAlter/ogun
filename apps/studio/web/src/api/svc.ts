// The hosted pre-alpha's service (`deploy/pre-alpha/svc`): the Studio screens that ogund does not implement yet,
// served on the same origin under `/svc/v1`. Every call is one POST of `{method, params}`, so the browser (fetch
// with the session cookie and the CSRF header) and the desktop shell (a Tauri command that adds the hub token)
// share this file. Where the service is absent, nothing here is used and Studio shows those pages as unavailable.
//
// What it holds is not the daemon's: agent threads (O-17, proposed; the replies are scripted, the tools call the
// real daemon), connections, run defaults, the provider ladder, sources, evidence, storage rules and the archive.
// The About page says so.

import type { AgentApi, Backend, EventSubscription } from './backend'
import { asOgunError } from './backend'
import type { AgentPreview, AgentThread, AgentThreadSummary, LadderResult } from './types'

export interface SvcTransport {
  call<T>(method: string, params?: Record<string, unknown>): Promise<T>
}

/** `GET /svc/v1/info`: what the service serves and how visitors sign in. */
export interface SvcInfo {
  service: string
  version: string
  release: string
  visitors: { mode: 'shared_account' | 'per_visitor'; reset: string; namespace_ttl_hours: number; sample_login?: string; sample_password?: string }
  product_server: string[]
  this_service: string[]
  agent: { replies: string; tools: string }
  sandbox: boolean
}

let infoCache: Promise<SvcInfo | null> | null = null

/** The service's description, or null where there is no service (a self-hosted hub, the offline build). */
export function svcInfo(): Promise<SvcInfo | null> {
  if (typeof fetch === 'undefined' || typeof window === 'undefined') return Promise.resolve(null)
  infoCache ??= fetch('/svc/v1/info', { credentials: 'same-origin', headers: { accept: 'application/json' } })
    .then(async (r) => (r.ok && (r.headers.get('content-type') ?? '').includes('json') ? ((await r.json()) as SvcInfo) : null))
    .then((i) => (i && i.service === 'ogun-prealpha-svc' ? i : null))
    .catch(() => null)
  return infoCache
}

/** The browser's transport: same origin, the session cookie, and the CSRF token on every call. */
export function fetchTransport(csrf: () => string | null): SvcTransport {
  return {
    async call<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
      const headers: Record<string, string> = { accept: 'application/json', 'content-type': 'application/json' }
      const token = csrf()
      if (token) headers['x-ogun-csrf'] = token
      let res: Response
      try {
        res = await fetch('/svc/v1/call', { method: 'POST', headers, credentials: 'same-origin', body: JSON.stringify({ method, params }) })
      } catch {
        throw asOgunError({ code: 'api.unavailable', class: 'unavailable', message: 'cannot reach the service', retryable: true })
      }
      let json: unknown = null
      try {
        json = await res.json()
      } catch {
        json = null
      }
      if (!res.ok) {
        if (json && typeof json === 'object' && 'error' in json) throw asOgunError((json as { error: unknown }).error)
        throw asOgunError({ code: 'api.unavailable', message: `the service answered HTTP ${res.status}` })
      }
      return json as T
    },
  }
}

/** Repeats a long-polling call until closed; `onValue` sees every answer. */
function poll<T extends { rev: number }>(t: SvcTransport, method: string, params: Record<string, unknown>, onValue: (v: T) => void): EventSubscription {
  let closed = false
  let since: number | undefined
  const loop = async () => {
    let failures = 0
    while (!closed) {
      try {
        const v = await t.call<T>(method, { ...params, ...(since !== undefined ? { since } : {}) })
        failures = 0
        if (closed) return
        since = v.rev
        onValue(v)
      } catch (e) {
        // A daemon that has no such method will not grow one while this page is open: stop asking.
        if (asOgunError(e).code === 'api.method_not_found') return
        failures++
        await new Promise((r) => setTimeout(r, Math.min(15_000, 1000 * failures)))
      }
    }
  }
  void loop()
  return {
    close() {
      closed = true
    },
  }
}

function agentApi(t: SvcTransport, preview: boolean): AgentApi {
  return {
    ...(preview ? { preview: (threadId: string | null, text: string) => t.call<AgentPreview>('agent.preview', { ...(threadId ? { thread_id: threadId } : {}), text }) } : {}),
    models: () => t.call('agent.models'),
    tools: () => t.call('agent.tools'),
    settings: () => t.call('agent.settings'),
    setSettings: (patch) => t.call('agent.set_settings', { patch }),
    watchThreads: (onChange) => poll<{ rev: number; threads: AgentThreadSummary[] }>(t, 'agent.threads', {}, (v) => onChange(v.threads)),
    watchThread: (threadId, onChange) => poll<{ rev: number; thread: AgentThread | null }>(t, 'agent.thread', { thread_id: threadId }, (v) => onChange(v.thread)),
    create: (opts) => t.call('agent.create', { access: opts.access, model: opts.model }),
    send: (threadId, text, mentions) => t.call('agent.send', { thread_id: threadId, text, mentions }),
    stop: (threadId) => t.call('agent.stop', { thread_id: threadId }),
    act: (threadId, itemId, action, idempotency_key) => t.call('agent.act', { thread_id: threadId, item_id: itemId, action, idempotency_key }),
    setAccess: (threadId, access) => t.call('agent.set_access', { thread_id: threadId, access }),
    rename: (threadId, title) => t.call('agent.rename', { thread_id: threadId, title }),
    remove: (threadId) => t.call('agent.remove', { thread_id: threadId }),
  }
}

/** The `Backend` members the service provides. The product server's members are never replaced. `preview`: the
 *  daemon can describe the next model request (`agent.preview`), which only the desktop's own engine does. */
export function svcParts(t: SvcTransport, opts: { preview?: boolean } = {}): Pick<Backend, 'agent' | 'connections' | 'runtime' | 'environments' | 'setLadder' | 'sources' | 'evidence' | 'storage' | 'archive' | 'secretsList' | 'llmLadder'> {
  return {
    agent: agentApi(t, opts.preview === true),
    connections: {
      list: () => t.call('connections.list'),
      put: (input) => t.call('connections.put', { input }),
      test: (name) => t.call('connections.test', { name }),
      remove: (name) => t.call('connections.remove', { name }),
    },
    runtime: {
      get: (workflow) => t.call('runtime.get', workflow ? { workflow } : {}),
      set: (workflow, patch) => t.call('runtime.set', { workflow, patch }),
      reset: (workflow, field) => t.call('runtime.reset', { workflow, field }),
    },
    environments: () => t.call('environments'),
    setLadder: (providers) => t.call('ladder.put', { providers }),
    sources: {
      get: () => t.call('sources.get'),
      setRepo: (id, patch) => t.call('sources.set_repo', { id, patch }),
      sync: (id) => t.call('sources.sync', { id }),
      setProvider: (id, enabled) => t.call('sources.set_provider', { id, enabled }),
      setAutoPull: (on) => t.call('sources.set_auto_pull', { on }),
    },
    evidence: {
      get: () => t.call('evidence.get'),
      set: (patch) => t.call('evidence.set', { patch }),
    },
    storage: {
      info: () => t.call('storage.info'),
      setRetention: (days) => t.call('storage.set_retention', { days }),
      clearCaches: () => t.call('storage.clear_caches'),
      rules: () => t.call('storage.rules'),
      setRules: (patch) => t.call('storage.set_rules', { patch }),
      cleanupPlan: () => t.call('storage.cleanup_plan'),
      cleanup: () => t.call('storage.cleanup'),
    },
    archive: {
      list: () => t.call('archive.list'),
      restore: (id) => t.call('archive.restore', { id }),
    },
    secretsList: () => t.call('secrets.list'),
    llmLadder: () => t.call<LadderResult>('ladder.get'),
  }
}
