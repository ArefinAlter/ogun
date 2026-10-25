// ogun-prealpha-svc: the hosted pre-alpha's service for the Studio screens that ogund does not implement yet.
//
// One HTTP listener inside the compose network, mounted by the host's reverse proxy at `/svc/` on the same origin
// as Studio-web. Every call is `POST /svc/v1/call {method, params}` (or `GET /svc/v1/info`), so the browser and the
// desktop shell use one tiny transport. Who is calling is decided by ogun-server, never here: the service forwards
// the caller's own session cookie or API token to `GET /api/v1/me` and uses the answer.
//
// What it holds, per visitor namespace (a browser's anonymous cookie, or a desktop install with a token): agent
// threads (O-17, proposed; the replies are scripted, every tool call goes to the real daemon with the service's
// own API token, which acts as an agent), data connections, run defaults, provider ladder, workflow sources,
// evidence, storage rules and the archive. Namespaces expire after 24 hours without a visit, and everything is
// cleared by the nightly reset.

import { createHash, randomBytes } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'

import type { Backend } from '../../../../apps/studio/web/src/api/backend'
import { asOgunError } from '../../../../apps/studio/web/src/api/backend'
import type {
  AgentAccess,
  AgentAction,
  AgentThread,
  AgentThreadSummary,
  ConnectionInput,
  EnvironmentsResult,
  LadderProvider,
  Mention,
  RuntimeSettings,
  StatusResult,
} from '../../../../apps/studio/web/src/api/types'
import { createScriptedAgent, type AgentSnapshot, type HostedAgent } from '../../../../apps/studio/web/src/demo/agent'
import { DemoExtras } from '../../../../apps/studio/web/src/demo/extras'
import { restBackend, whoIs, type Credential, type Me, type RestTarget } from './rest'
import { Store, deserialize, serialize } from './store'

const VERSION = process.env.OGUN_PREALPHA_VERSION ?? '0.0.1-alpha'
const PORT = Number(process.env.SVC_PORT ?? 8090)
const HUB = process.env.OGUN_SERVER_URL ?? 'http://ogun-server:8080'
const PUBLIC_URL = process.env.OGUN_PUBLIC_URL ?? 'https://ogun.103.191.50.32.sslip.io'
const PUBLIC_HOST = new URL(PUBLIC_URL).host
const DB = process.env.SVC_DB ?? '/data/svc.db'
const TOKEN_FILE = process.env.SVC_AGENT_TOKEN_FILE ?? '/run/secrets/agent-token'
const SAMPLE_LOGIN = process.env.SVC_SAMPLE_LOGIN ?? ''
const SAMPLE_PASSWORD_FILE = process.env.SVC_SAMPLE_PASSWORD_FILE ?? ''
const RESET_AT = process.env.SVC_RESET_AT ?? '03:00 UTC'
const NS_TTL_MS = 24 * 3_600_000
const MAX_NAMESPACES = 5000
const MAX_LOADED = 300
const MAX_BODY = 64 * 1024
const MAX_THREADS = 30
const MAX_ITEMS = 400
const MAX_DOC_BYTES = 2 * 1024 * 1024
const VISITOR_COOKIE = 'ogun_pa_vid'
const ME_CACHE_MS = 20_000

const store = new Store(DB)

function readSecret(path: string): string | null {
  if (!path) return null
  try {
    const v = readFileSync(path, 'utf8').trim()
    return v || null
  } catch {
    return null
  }
}

// ---------------------------------------------------------------- namespaces

interface Saved {
  extras?: string
  agent?: AgentSnapshot
}

interface Ns {
  id: string
  extras: DemoExtras
  agent: HostedAgent
  rev: number
  waiters: Set<() => void>
  dirty: boolean
  timer: NodeJS.Timeout | null
  lastUse: number
}

const loaded = new Map<string, Ns>()

function agentCred(): Credential | null {
  const token = readSecret(TOKEN_FILE)
  return token ? { kind: 'token', token } : null
}

function hubTarget(forwardedFor?: string): RestTarget {
  return { base: HUB, host: PUBLIC_HOST, ...(forwardedFor ? { forwardedFor } : {}) }
}

/** The agent's own backend: the service's API token, which always acts as an agent. */
function agentBackend(): Backend {
  const cred = agentCred()
  if (!cred) {
    const refuse = () => Promise.reject(asOgunError({ code: 'api.unavailable', message: 'the agent has no credential on this hub yet' }))
    return new Proxy({} as Backend, { get: (_t, k) => (k === 'kind' ? 'http' : refuse) })
  }
  return restBackend(hubTarget(), cred)
}

const NO_HOST = {
  workflows: () => [],
  runs: () => new Map(),
  put: () => {
    throw asOgunError({ code: 'api.method_not_found', message: 'workflows are stored on the daemon' })
  },
  guard: () => {},
  notify: () => {},
}

function restoreExtras(saved?: string): DemoExtras {
  const x = new DemoExtras(NO_HOST)
  if (saved) {
    const data = deserialize<Record<string, unknown>>(saved)
    for (const [k, v] of Object.entries(data)) if (k !== 'host') (x as unknown as Record<string, unknown>)[k] = v
  }
  return x
}

function flush(ns: Ns): void {
  if (ns.timer) {
    clearTimeout(ns.timer)
    ns.timer = null
  }
  if (!ns.dirty) return
  ns.dirty = false
  const extras: Record<string, unknown> = {}
  for (const [k, v] of Object.entries(ns.extras)) if (k !== 'host') extras[k] = v
  const snap = ns.agent.snapshot()
  // Bound what one visitor can store: the oldest threads go first, then the oldest items of a long thread.
  snap.threads.sort((a, b) => b.updated_at.localeCompare(a.updated_at))
  snap.threads = snap.threads.slice(0, MAX_THREADS)
  for (const t of snap.threads) if (t.items.length > MAX_ITEMS) t.items = t.items.slice(-MAX_ITEMS)
  let doc = serialize({ extras: serialize(extras), agent: snap } satisfies Saved)
  if (doc.length > MAX_DOC_BYTES) {
    snap.threads = snap.threads.slice(0, 5)
    doc = serialize({ extras: serialize(extras), agent: snap } satisfies Saved)
  }
  store.put(ns.id, doc)
}

function saveNs(ns: Ns): void {
  ns.dirty = true
  if (!ns.timer) ns.timer = setTimeout(() => flush(ns), 400)
}

function changed(ns: Ns): void {
  ns.rev++
  for (const w of ns.waiters) w()
  ns.waiters.clear()
  saveNs(ns)
}

function namespace(id: string): Ns {
  const hit = loaded.get(id)
  if (hit) {
    hit.lastUse = Date.now()
    return hit
  }
  if (loaded.size >= MAX_LOADED) {
    const oldest = [...loaded.values()].sort((a, b) => a.lastUse - b.lastUse)[0]
    if (oldest) {
      flush(oldest)
      loaded.delete(oldest.id)
    }
  }
  const row = store.get(id)
  if (!row && store.count() >= MAX_NAMESPACES) throw asOgunError({ code: 'api.rate_limited', class: 'unavailable', message: 'the hosted pre-alpha is full right now; try again later', retryable: true })
  const saved: Saved = row ? deserialize<Saved>(row.data) : {}
  const ns: Ns = { id, extras: restoreExtras(saved.extras), rev: 1, waiters: new Set(), dirty: false, timer: null, lastUse: Date.now(), agent: undefined as unknown as HostedAgent }
  ns.agent = createScriptedAgent(agentBackend, {
    ...(saved.agent ? { restore: saved.agent } : {}),
    onChange: () => changed(ns),
    instant: () => false,
  })
  loaded.set(id, ns)
  if (row) store.touch(id)
  else saveNs(ns)
  return ns
}

// ---------------------------------------------------------------- callers

interface Caller {
  me: Me
  cred: Credential
  ns: string
  canWrite: boolean
  setCookie?: string
}

const meCache = new Map<string, { at: number; me: Me | null }>()

function cookies(req: IncomingMessage): Map<string, string> {
  const out = new Map<string, string>()
  for (const part of (req.headers.cookie ?? '').split(';')) {
    const i = part.indexOf('=')
    if (i > 0) out.set(part.slice(0, i).trim(), part.slice(i + 1).trim())
  }
  return out
}

function clientIp(req: IncomingMessage): string {
  const xff = String(req.headers['x-forwarded-for'] ?? '').split(',')[0]?.trim()
  return xff || req.socket.remoteAddress || ''
}

const ROLE_RANK: Record<string, number> = { viewer: 0, editor: 1, admin: 2, owner: 3 }

async function caller(req: IncomingMessage): Promise<Caller> {
  const jar = cookies(req)
  const auth = String(req.headers.authorization ?? '')
  let cred: Credential
  if (/^Bearer\s+\S+$/i.test(auth)) cred = { kind: 'token', token: auth.replace(/^Bearer\s+/i, '') }
  else {
    // Forward the hub's own cookies only; the service's visitor cookie stays here.
    const forward = [...jar.entries()].filter(([k]) => k !== VISITOR_COOKIE).map(([k, v]) => `${k}=${v}`).join('; ')
    if (!forward) throw asOgunError({ code: 'api.unauthenticated', class: 'unauthenticated', message: 'sign in first' })
    cred = { kind: 'session', cookie: forward, csrf: null }
  }
  const key = createHash('sha256').update(cred.kind === 'token' ? 't:' + cred.token : 's:' + cred.cookie).digest('hex')
  let entry = meCache.get(key)
  if (!entry || Date.now() - entry.at > ME_CACHE_MS) {
    entry = { at: Date.now(), me: await whoIs(hubTarget(clientIp(req)), cred) }
    meCache.set(key, entry)
    if (meCache.size > 2000) for (const k of [...meCache.keys()].slice(0, 500)) meCache.delete(k)
  }
  const me = entry.me
  if (!me) throw asOgunError({ code: 'api.unauthenticated', class: 'unauthenticated', message: 'sign in first' })
  if (cred.kind === 'session') cred.csrf = me.csrf_token ?? null
  const role = me.kind === 'token' ? (me.token?.role ?? 'viewer') : (me.workspaces?.[0]?.role ?? 'viewer')
  const out: Caller = { me, cred, ns: '', canWrite: (ROLE_RANK[role] ?? 0) >= ROLE_RANK.editor! }
  if (me.kind === 'token') {
    const install = String(req.headers['x-ogun-install'] ?? '').replace(/[^A-Za-z0-9_-]/g, '').slice(0, 64)
    out.ns = `t:${me.token?.token_id ?? 'token'}:${install || 'default'}`
  } else {
    let vid = jar.get(VISITOR_COOKIE)
    if (!vid || !/^[A-Za-z0-9_-]{22,64}$/.test(vid)) {
      vid = randomBytes(18).toString('base64url')
      out.setCookie = `${VISITOR_COOKIE}=${vid}; Path=/svc/; Max-Age=${NS_TTL_MS / 1000}; HttpOnly; Secure; SameSite=Strict`
    }
    out.ns = `v:${vid}`
  }
  return out
}

// ---------------------------------------------------------------- methods

type Params = Record<string, unknown>
type Method = (c: Caller, ns: Ns, p: Params) => Promise<unknown> | unknown

const str = (v: unknown, what: string, max = 200): string => {
  if (typeof v !== 'string' || v.length === 0 || v.length > max) throw asOgunError({ code: 'api.invalid_params', class: 'invalid_argument', message: `${what} is missing or too long` })
  return v
}
const obj = (v: unknown, what: string): Params => {
  if (!v || typeof v !== 'object' || Array.isArray(v)) throw asOgunError({ code: 'api.invalid_params', class: 'invalid_argument', message: `${what} must be an object` })
  return v as Params
}
const ACCESS: AgentAccess[] = ['read', 'author', 'operate']
const access = (v: unknown): AgentAccess => {
  if (!ACCESS.includes(v as AgentAccess)) throw asOgunError({ code: 'api.invalid_params', class: 'invalid_argument', message: 'access must be read, author or operate' })
  return v as AgentAccess
}

/** Waits until the namespace changes after `since`, or up to `ms`. */
function waitFor(ns: Ns, since: number | undefined, ms = 20_000): Promise<void> {
  if (since === undefined || since !== ns.rev) return Promise.resolve()
  return new Promise((resolve) => {
    const t = setTimeout(done, ms)
    function done() {
      clearTimeout(t)
      ns.waiters.delete(wake)
      resolve()
    }
    // Coalesce a burst of streamed words into one answer.
    function wake() {
      setTimeout(done, 120)
    }
    ns.waiters.add(wake)
  })
}

function latestThreads(ns: Ns): Promise<AgentThreadSummary[]> {
  return new Promise((resolve) => {
    const sub = ns.agent.watchThreads((t) => {
      resolve(t)
      queueMicrotask(() => sub.close())
    })
  })
}

function latestThread(ns: Ns, id: string): Promise<AgentThread | null> {
  return new Promise((resolve) => {
    let done = false
    const sub = ns.agent.watchThread(id, (t) => {
      if (done) return
      done = true
      resolve(t)
      queueMicrotask(() => sub.close())
    })
  })
}

async function hubStatus(): Promise<StatusResult | null> {
  try {
    return await agentBackend().status()
  } catch {
    return null
  }
}

async function environments(c: Caller): Promise<EnvironmentsResult> {
  const backend = restBackend(hubTarget(), c.cred)
  const [status, edges] = await Promise.all([hubStatus(), backend.edgesList().catch(() => ({ edges: [] }))])
  return {
    scheduler_enabled: true,
    environments: [
      {
        id: 'local',
        name: 'This hub',
        kind: 'hub',
        connected: !!status,
        version: (status as { version?: string } | null)?.version ?? VERSION,
        endpoint: PUBLIC_URL,
        detail: 'ogund --profile server, shared sample workspace, reset every night',
      },
      ...edges.edges.map((e) => ({
        id: e.edge_id,
        name: e.name,
        kind: 'edge' as const,
        connected: !!e.last_sync_at && Date.now() - Date.parse(e.last_sync_at) < 15 * 60_000,
        detail: `enrolled ${String(e.enrolled_at).slice(0, 10)}`,
        ...(e.last_sync_at ? { last_seen_at: e.last_sync_at } : {}),
      })),
    ],
    bridges: [
      { id: 'server', name: 'Server (ogun-server)', state: 'running', detail: `behind a reverse proxy at ${PUBLIC_URL} · OIDC sign-in (Dex) · one shared sample account` },
      { id: 'svc', name: 'Pre-alpha service', state: 'running', detail: 'agent threads, connections and the settings ogund does not implement yet, per visitor, kept 24 hours' },
      { id: 'sync', name: 'Edge-to-hub sync', state: edges.edges.length ? 'running' : 'not_configured', detail: edges.edges.length ? `${edges.edges.length} enrolled edge${edges.edges.length === 1 ? '' : 's'}` : 'no edge is enrolled on this hub' },
      { id: 'sandbox', name: 'Operator sandbox', state: 'not_configured', detail: 'Not installed on this hub: code operators (python, sql, dbt, shell, polars) cannot run here.' },
      { id: 'zeus', name: 'Zeus session bridge', state: 'not_configured', detail: 'Not linked on this hub. Browser-session sources go through zeusd.' },
      { id: 'remote_approvals', name: 'Remote approvals (WebAuthn)', state: 'locked', detail: 'Off. ADR 0010 requires an external security review first; approvals here are decided on the host terminal.' },
    ],
  } as EnvironmentsResult
}

const READ: Record<string, Method> = {
  'agent.models': (_c, ns) => ns.agent.models(),
  'agent.tools': (_c, ns) => ns.agent.tools(),
  'agent.settings': (_c, ns) => ns.agent.settings(),
  'agent.threads': async (_c, ns, p) => {
    await waitFor(ns, typeof p.since === 'number' ? p.since : undefined)
    return { rev: ns.rev, threads: await latestThreads(ns) }
  },
  'agent.thread': async (_c, ns, p) => {
    const id = str(p.thread_id, 'thread_id', 64)
    await waitFor(ns, typeof p.since === 'number' ? p.since : undefined)
    return { rev: ns.rev, thread: await latestThread(ns, id) }
  },
  'connections.list': (_c, ns) => ({ connections: ns.extras.connectionsList() }),
  'runtime.get': (_c, ns, p) => ns.extras.runtimeGet(typeof p.workflow === 'string' ? p.workflow : undefined),
  environments: (c) => environments(c),
  'ladder.get': (_c, ns) => ({ available: true, ladder: { version: 1, providers: ns.extras.ladderGet() }, spend: { month_usd_micro: 0, cap_usd_micro: 25_000_000, entries: 0 } }),
  'sources.get': (_c, ns) => ns.extras.sourcesGet(),
  'evidence.get': (_c, ns) => ns.extras.evidenceGet(),
  'storage.info': (_c, ns) => ns.extras.storageInfo(),
  'storage.rules': (_c, ns) => ns.extras.storageRules(),
  'storage.cleanup_plan': (_c, ns) => ns.extras.storageCleanupPlan(),
  'archive.list': (_c, ns) => ({ items: ns.extras.archiveList() }),
  'secrets.list': (_c, ns) => ({ names: ns.extras.secretNames() }),
}

const WRITE: Record<string, Method> = {
  'agent.set_settings': (_c, ns, p) => ns.agent.setSettings(obj(p.patch, 'patch'), ''),
  'agent.create': async (_c, ns, p) => {
    const list = await latestThreads(ns)
    if (list.length >= MAX_THREADS) throw asOgunError({ code: 'api.rate_limited', class: 'unavailable', message: `at most ${MAX_THREADS} threads; delete one first` })
    return ns.agent.create({ access: access(p.access), model: str(p.model, 'model', 64) })
  },
  'agent.send': (_c, ns, p) => ns.agent.send(str(p.thread_id, 'thread_id', 64), str(p.text, 'text', 4000), Array.isArray(p.mentions) ? (p.mentions.slice(0, 16) as Mention[]) : []),
  'agent.stop': (_c, ns, p) => ns.agent.stop(str(p.thread_id, 'thread_id', 64)),
  // A card's button is a person's act, so it runs with that person's own session or token, not the agent's.
  'agent.act': (c, ns, p) =>
    ns.agent.act(str(p.thread_id, 'thread_id', 64), str(p.item_id, 'item_id', 64), str(p.action, 'action', 32) as AgentAction, str(p.idempotency_key, 'idempotency_key', 128), restBackend(hubTarget(), c.cred)),
  'agent.set_access': (_c, ns, p) => ns.agent.setAccess(str(p.thread_id, 'thread_id', 64), access(p.access)),
  'agent.rename': (_c, ns, p) => ns.agent.rename(str(p.thread_id, 'thread_id', 64), str(p.title, 'title', 200)),
  'agent.remove': (_c, ns, p) => ns.agent.remove(str(p.thread_id, 'thread_id', 64)),
  'connections.put': (_c, ns, p) => {
    if (ns.extras.connectionsList().length >= 60) throw asOgunError({ code: 'api.rate_limited', class: 'unavailable', message: 'at most 60 connections' })
    return ns.extras.connectionPut(obj(p.input, 'input') as unknown as ConnectionInput)
  },
  'connections.test': (_c, ns, p) => ns.extras.connectionTest(str(p.name, 'name', 64)),
  'connections.remove': (_c, ns, p) => ns.extras.connectionRemove(str(p.name, 'name', 64)),
  'runtime.set': (_c, ns, p) => ns.extras.runtimeSet(typeof p.workflow === 'string' ? p.workflow : null, obj(p.patch, 'patch') as Partial<RuntimeSettings>),
  'runtime.reset': (_c, ns, p) => ns.extras.runtimeReset(str(p.workflow, 'workflow', 128), str(p.field, 'field', 64) as keyof RuntimeSettings),
  'ladder.put': (_c, ns, p) => {
    if (!Array.isArray(p.providers) || p.providers.length > 32) throw asOgunError({ code: 'api.invalid_params', class: 'invalid_argument', message: 'providers must be a list of at most 32' })
    // Keys never come here: Studio keeps a typed key in the page's memory only, and the ladder names a secret.
    for (const pr of p.providers as Record<string, unknown>[]) delete pr.api_key
    ns.extras.ladderPut(p.providers as LadderProvider[])
  },
  'sources.set_repo': (_c, ns, p) => ns.extras.sourcesSetRepo(str(p.id, 'id', 64), obj(p.patch, 'patch') as never),
  'sources.sync': (_c, ns, p) => ns.extras.sourcesSync(str(p.id, 'id', 64)),
  'sources.set_provider': (_c, ns, p) => ns.extras.sourcesSetProvider(str(p.id, 'id', 64), p.enabled === true),
  'sources.set_auto_pull': (_c, ns, p) => ns.extras.sourcesAutoPull(p.on === true),
  'evidence.set': (_c, ns, p) => ns.extras.evidenceSet(obj(p.patch, 'patch')),
  'storage.set_retention': (_c, ns, p) => {
    ns.extras.storageSetRules({ retention_days: Number(p.days) })
    return ns.extras.storageInfo()
  },
  'storage.clear_caches': (_c, ns) => ns.extras.storageClearCaches(),
  'storage.set_rules': (_c, ns, p) => ns.extras.storageSetRules(obj(p.patch, 'patch')),
  'storage.cleanup': (_c, ns) => ns.extras.storageCleanup(),
  'archive.restore': (_c, ns, p) => ns.extras.archiveRestore(str(p.id, 'id', 64)),
}

// ---------------------------------------------------------------- HTTP

const SECURITY_HEADERS: Record<string, string> = {
  'content-type': 'application/json; charset=utf-8',
  'cache-control': 'no-store',
  'x-content-type-options': 'nosniff',
  'referrer-policy': 'no-referrer',
  'x-robots-tag': 'noindex, nofollow',
}

function send(res: ServerResponse, status: number, body: unknown, extra: Record<string, string> = {}): void {
  res.writeHead(status, { ...SECURITY_HEADERS, ...extra })
  res.end(JSON.stringify(body))
}

const STATUS_OF: Record<string, number> = {
  'api.unauthenticated': 401,
  'api.permission_denied': 403,
  'api.method_not_found': 404,
  'api.not_found': 404,
  'connection.not_found': 404,
  'archive.not_found': 404,
  'api.conflict': 409,
  'api.rate_limited': 429,
  'api.unavailable': 503,
}

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    let size = 0
    const chunks: Buffer[] = []
    req.on('data', (c: Buffer) => {
      size += c.length
      if (size > MAX_BODY) {
        reject(asOgunError({ code: 'api.invalid_params', class: 'invalid_argument', message: 'the request is too large' }))
        req.destroy()
      } else chunks.push(c)
    })
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    req.on('error', reject)
  })
}

// A simple per-address budget, on top of nginx's limit_req.
const buckets = new Map<string, { at: number; n: number }>()
function allow(ip: string, cost = 1): boolean {
  const now = Date.now()
  const b = buckets.get(ip)
  if (!b || now - b.at > 60_000) {
    buckets.set(ip, { at: now, n: cost })
    if (buckets.size > 10_000) buckets.clear()
    return true
  }
  b.n += cost
  return b.n <= 600
}

function info() {
  const password = readSecret(SAMPLE_PASSWORD_FILE)
  return {
    service: 'ogun-prealpha-svc',
    version: VERSION,
    release: 'Pre-alpha release',
    visitors: {
      mode: 'shared_account',
      reset: `every night at ${RESET_AT}`,
      namespace_ttl_hours: NS_TTL_MS / 3_600_000,
      ...(SAMPLE_LOGIN && password ? { sample_login: SAMPLE_LOGIN, sample_password: password } : {}),
    },
    product_server: ['workflows', 'runs', 'partitions', 'backfills', 'streams', 'edges', 'approvals', 'artifacts', 'lineage', 'schedules', 'sign-in'],
    this_service: ['agent threads', 'connections', 'run defaults', 'environments overview', 'provider ladder', 'workflow sources', 'evidence', 'storage rules', 'archive', 'secret names'],
    agent: { replies: 'scripted, no model is called', tools: 'the real daemon, with an API token that acts as an agent' },
    sandbox: false,
  }
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url ?? '/', 'http://svc')
  const path = url.pathname.replace(/^\/svc/, '')
  const ip = clientIp(req)
  try {
    if (req.method === 'GET' && path === '/v1/info') return send(res, 200, info())
    if (req.method === 'GET' && path === '/healthz') return send(res, 200, { ok: true })
    if (req.method !== 'POST' || path !== '/v1/call') return send(res, 404, { error: { code: 'api.method_not_found', class: 'not_found', message: 'no such route', retryable: false } })
    if (!allow(ip)) return send(res, 429, { error: { code: 'api.rate_limited', class: 'unavailable', message: 'too many requests; slow down', retryable: true } })
    if (!String(req.headers['content-type'] ?? '').includes('application/json')) throw asOgunError({ code: 'api.invalid_params', class: 'invalid_argument', message: 'send JSON' })
    const body = obj(JSON.parse((await readBody(req)) || '{}'), 'body')
    const method = str(body.method, 'method', 64)
    const params = body.params === undefined ? {} : obj(body.params, 'params')
    const c = await caller(req)
    const write = WRITE[method]
    const read = READ[method]
    if (!write && !read) throw asOgunError({ code: 'api.method_not_found', class: 'not_found', message: `no method ${method}` })
    if (write) {
      if (!c.canWrite) throw asOgunError({ code: 'api.permission_denied', class: 'permission_denied', message: 'this needs the editor role' })
      // Same CSRF rule as ogun-server: a browser session sends its CSRF token on every change.
      if (c.cred.kind === 'session' && (!c.cred.csrf || req.headers['x-ogun-csrf'] !== c.cred.csrf)) throw asOgunError({ code: 'api.permission_denied', class: 'permission_denied', message: 'missing or wrong CSRF token' })
    }
    const ns = namespace(c.ns)
    const out = await (write ?? read)!(c, ns, params)
    if (write) changed(ns)
    send(res, 200, out ?? { ok: true }, c.setCookie ? { 'set-cookie': c.setCookie } : {})
  } catch (e) {
    const err = asOgunError(e instanceof SyntaxError ? { code: 'api.invalid_params', message: 'the body is not JSON' } : e)
    send(res, STATUS_OF[err.code] ?? (err.class === 'internal' ? 500 : 400), { error: err })
  }
})

server.requestTimeout = 30_000
server.headersTimeout = 10_000
server.listen(PORT, '0.0.0.0', () => {
  console.log(JSON.stringify({ msg: 'ogun-prealpha-svc listening', port: PORT, version: VERSION }))
})

// Expiry, hourly. The nightly reset (deploy/pre-alpha/reset.sh) stops the service and deletes the database file.
setInterval(() => {
  const n = store.expire(Date.now() - NS_TTL_MS)
  for (const [id, ns] of loaded) if (Date.now() - ns.lastUse > NS_TTL_MS) loaded.delete(id)
  if (n) console.log(JSON.stringify({ msg: 'expired namespaces', count: n }))
}, 3_600_000).unref()

process.on('SIGTERM', () => {
  for (const ns of loaded.values()) flush(ns)
  server.close(() => process.exit(0))
  setTimeout(() => process.exit(0), 3000).unref()
})
