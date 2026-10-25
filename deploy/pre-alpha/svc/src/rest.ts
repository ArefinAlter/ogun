// A `Backend` for Node that talks to ogun-server's REST routes (O-13), the same routes Studio-web uses. Two kinds
// of caller use it: the agent, with the service's own API token (a token always acts as an agent, so it can never
// decide an approval), and a person's button press on a card, with that person's own session cookie and CSRF token
// forwarded unchanged. Only the methods the agent needs are implemented; the rest refuse.

import type { Backend } from '../../../../apps/studio/web/src/api/backend'
import { asOgunError } from '../../../../apps/studio/web/src/api/backend'

/** Who the calls act as. */
export type Credential =
  | { kind: 'token'; token: string }
  | { kind: 'session'; cookie: string; csrf: string | null }

export interface RestTarget {
  /** Base URL of ogun-server inside the network, e.g. `http://ogun-server:8080`. */
  base: string
  /** The `Host` header ogun-server expects (the host of its `public_url`). */
  host: string
  /** The visitor's address, forwarded for rate limits. */
  forwardedFor?: string
}

const TIMEOUT_MS = 20_000
const MAX_BYTES = 8 * 1024 * 1024

function query(params: Record<string, string | number | undefined | null>): string {
  const q = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== '') q.set(k, String(v))
  const s = q.toString()
  return s ? '?' + s : ''
}

const p = (...segments: string[]) => '/api/v1/' + segments.map(encodeURIComponent).join('/')

export async function rawCall(
  t: RestTarget,
  cred: Credential,
  method: 'GET' | 'POST',
  path: string,
  body?: unknown,
): Promise<{ status: number; json: unknown; bytes?: Uint8Array }> {
  const headers: Record<string, string> = { accept: 'application/json', host: t.host, 'x-forwarded-proto': 'https' }
  if (t.forwardedFor) headers['x-forwarded-for'] = t.forwardedFor
  if (cred.kind === 'token') headers.authorization = `Bearer ${cred.token}`
  else {
    headers.cookie = cred.cookie
    if (method !== 'GET' && cred.csrf) headers['x-ogun-csrf'] = cred.csrf
  }
  if (body !== undefined) headers['content-type'] = 'application/json'
  const ctl = new AbortController()
  const timer = setTimeout(() => ctl.abort(), TIMEOUT_MS)
  try {
    const res = await fetch(t.base + path, {
      method,
      headers,
      signal: ctl.signal,
      redirect: 'manual',
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
    })
    const buf = new Uint8Array(await res.arrayBuffer())
    if (buf.byteLength > MAX_BYTES) throw asOgunError({ code: 'api.unavailable', message: 'the hub answered with too much data' })
    const ct = res.headers.get('content-type') ?? ''
    let json: unknown = null
    if (ct.includes('json')) {
      try {
        json = JSON.parse(new TextDecoder().decode(buf))
      } catch {
        json = null
      }
    }
    return { status: res.status, json, bytes: buf }
  } catch (e) {
    if (e && typeof e === 'object' && 'code' in e) throw e
    throw asOgunError({ code: 'api.unavailable', class: 'unavailable', message: 'cannot reach the hub', retryable: true })
  } finally {
    clearTimeout(timer)
  }
}

async function call<T>(t: RestTarget, cred: Credential, method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
  const r = await rawCall(t, cred, method, path, body)
  if (r.status < 200 || r.status >= 300) {
    if (r.json && typeof r.json === 'object' && 'error' in r.json) throw asOgunError((r.json as { error: unknown }).error)
    throw asOgunError({ code: 'api.unavailable', message: `the hub answered HTTP ${r.status}` })
  }
  return r.json as T
}

function refuse(what: string): never {
  throw asOgunError({ code: 'api.method_not_found', class: 'unsupported', message: `${what} is not available to the agent` })
}

export function restBackend(t: RestTarget, cred: Credential): Backend {
  const get = <T>(path: string) => call<T>(t, cred, 'GET', path)
  const post = <T>(path: string, body: unknown) => call<T>(t, cred, 'POST', path, body)
  return {
    kind: 'http',
    info: () => refuse('info'),
    status: () => get(p('status')),
    workflowsList: () => get(p('workflows')),
    workflowsGet: (name, version) => get(p('workflows', name) + query({ version })),
    workflowsValidate: (workflow) => post(p('workflows', 'validate'), { workflow }),
    workflowsPut: (workflow, idempotency_key) => post(p('workflows'), { workflow, idempotency_key }),
    runsList: (q) => get(p('runs') + query({ workflow: q.workflow, before: q.before, limit: q.limit })),
    runsGet: (runId) => get(p('runs', runId)),
    runsStart: (q) => {
      const { workflow, ...rest } = q
      return post(p('workflows', workflow, 'runs'), rest)
    },
    runsCancel: (runId, idempotency_key) => post(p('runs', runId, 'cancel'), { idempotency_key }),
    partitionsStatus: (q) => get(p('workflows', q.workflow, 'partitions') + query({ version: q.version, from: q.from, to: q.to })),
    backfillsPlan: (workflow, from, to) => post(p('workflows', workflow, 'backfill-plan'), { from, to }),
    subscribe: () => refuse('the event stream'),
    approvalsList: (state) => get(p('approvals') + query({ state })),
    approvalsGet: (id) => get(p('approvals', id)),
    prepareDecision: () => refuse('deciding an approval'),
    artifactsList: (runId) => get(p('artifacts') + query({ run_id: runId })),
    async artifactsGet(blake3, inlineMaxBytes) {
      const meta = await get<Record<string, unknown>>(p('artifacts', blake3))
      const result = ('artifact' in meta ? meta : { artifact: meta }) as { artifact: { size_bytes: number }; content_base64?: string }
      if (inlineMaxBytes > 0 && result.artifact.size_bytes <= inlineMaxBytes && !result.content_base64) {
        const r = await rawCall(t, cred, 'GET', p('artifacts', blake3, 'content'))
        if (r.status === 200 && r.bytes && r.bytes.byteLength <= inlineMaxBytes) result.content_base64 = Buffer.from(r.bytes).toString('base64')
      }
      return result as never
    },
    lineageGet: (q) => get(p('lineage') + query(q)),
    streamsList: () => get(p('streams')),
    streamsStatus: (workflow, node) => get(p('streams', workflow, node)),
    edgesList: () => get(p('edges')),
    credentialsList: () => refuse('credentials'),
    secretsList: () => refuse('secrets'),
    llmLadder: async () => ({ available: false }),
  }
}

/** `GET /api/v1/me` with the caller's own credential: who is asking, or null when nobody is signed in. */
export async function whoIs(t: RestTarget, cred: Credential): Promise<Me | null> {
  const r = await rawCall(t, cred, 'GET', p('me'))
  if (r.status !== 200 || !r.json || typeof r.json !== 'object') return null
  return r.json as Me
}

export interface Me {
  kind?: 'session' | 'token'
  user?: { user_id?: string; display_name?: string; email?: string }
  workspaces?: { workspace_id?: string; role?: string; approver?: boolean }[]
  token?: { token_id?: string; role?: string }
  csrf_token?: string
}
