import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import ts from 'typescript'

// Compile the real backend without a browser or additional test dependencies.
const dir = await mkdtemp(join(tmpdir(), 'ogun-http-'))
for (const name of ['http', 'backend', 'webauthn']) {
  const source = await readFile(new URL(`../src/api/${name}.ts`, import.meta.url), 'utf8')
  const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } })
  await writeFile(join(dir, `${name}.mjs`), outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"))
}
const { httpBackend } = await import(pathToFileURL(join(dir, 'http.mjs')).href)
after(() => rm(dir, { recursive: true, force: true }))
globalThis.__APP_VERSION__ = 'test'
globalThis.document = { cookie: '' }
globalThis.window = { PublicKeyCredential: class {} }
const memberships = [
  { workspace_id: 'wsp_editor', name: 'Authoring', role: 'editor', approver: true },
  { workspace_id: 'wsp_viewer', name: 'Read only', role: 'viewer', approver: false },
]
const me = { user: { user_id: 'usr_1', display_name: 'Ada' }, workspaces: memberships, csrf_token: 'csrf', remote_approvals: true }
let calls, streams, respond
class Source {
  static CLOSED = 2
  readyState = 1
  closed = false
  constructor(url) { this.url = url; streams.push(this) }
  addEventListener(_, callback) { this.callback = callback }
  close() { this.closed = true }
}
globalThis.EventSource = Source
function setup(payload = me) {
  calls = []; streams = []
  respond = (url) => url.endsWith('/me') ? payload : url.endsWith('/content') ? new Uint8Array([65]) : url.includes('/artifacts/') ? { artifact: { size_bytes: 1 } } : url.includes('/approvals/') ? { state: 'pending', request_hash: 'hash' } : { last_event_seq: 42 }
  globalThis.fetch = async (url, init) => {
    calls.push({ url, ...init })
    const value = await respond(url)
    return new Response(value instanceof Uint8Array ? value : JSON.stringify(value), { status: 200 })
  }
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { credentials: { get: async () => null } } })
}
const flush = () => new Promise((resolve) => setImmediate(resolve))

test('server-shaped /me maps identity and selected membership, and roles change with workspace', async () => {
  setup()
  const backend = httpBackend()
  const info = await backend.info()
  assert.equal(info.user.name, 'Ada')
  assert.deepEqual(info.user.roles, ['editor'])
  assert.equal(info.canWrite, true)
  assert.equal(info.decideChannel, 'webauthn')
  assert.equal(info.workspaceId, 'wsp_editor')
  assert.deepEqual(info.workspaces, memberships)
  assert.throws(() => backend.selectWorkspace('unknown'))
  const viewer = backend.selectWorkspace('wsp_viewer')
  const next = await viewer.info()
  assert.equal(next.canWrite, false)
  assert.equal(next.decideChannel, null)
  assert.deepEqual(next.user.roles, ['viewer'])
  assert.equal(calls.at(-1).headers['x-ogun-workspace'], 'wsp_viewer')
})

test('approval controls require explicitly enabled remote approvals, membership approver, and WebAuthn', async () => {
  for (const enabled of [undefined, false, true]) {
    setup({ ...me, remote_approvals: enabled })
    assert.equal((await httpBackend().info()).decideChannel, enabled === true ? 'webauthn' : null)
  }
  setup()
  globalThis.window = {}
  assert.equal((await httpBackend().info()).decideChannel, null)
  globalThis.window = { PublicKeyCredential: class {} }
})

test('all REST operations and artifact content explicitly address selected workspace', async () => {
  setup()
  const b = httpBackend('wsp_editor')
  await b.info()
  const actions = [
    () => b.status(), () => b.workflowsList(), () => b.workflowsGet('flow', 2),
    () => b.workflowsValidate({}), () => b.workflowsPut({}, 'key'),
    () => b.runsList({ workflow: 'flow', limit: 3 }), () => b.runsGet('run'),
    () => b.runsStart({ workflow: 'flow' }), () => b.runsCancel('run', 'key'),
    () => b.partitionsStatus({ workflow: 'flow' }), () => b.backfillsPlan('flow', 'a', 'b'),
    () => b.approvalsList('pending'), () => b.approvalsGet('approval'),
    () => b.prepareDecision('approval', 'approve'), () => b.artifactsList('run'),
    () => b.artifactsGet('hash', 5), () => b.lineageGet({ run_id: 'run' }),
    () => b.streamsList(), () => b.streamsStatus('flow', 'node'), () => b.edgesList(),
  ]
  for (const action of actions) await action()
  assert.ok(calls.some((c) => c.url.endsWith('/content')))
  for (const call of calls) {
    assert.equal(call.headers['x-ogun-workspace'], 'wsp_editor', call.url)
    if (call.method === 'POST') assert.equal(call.headers['x-ogun-csrf'], 'csrf')
  }
})

test('workspace changes close previous SSE and scope new stream URLs, preserving filters/cursor', async () => {
  setup()
  const b = httpBackend()
  await b.info()
  let events = 0
  b.subscribe({ runId: 'run 1' }, () => events++)
  await flush()
  const source = streams[0]
  const url = new URL(source.url, 'https://studio.test')
  assert.equal(url.searchParams.get('workspace'), 'wsp_editor')
  assert.equal(url.searchParams.get('run_id'), 'run 1')
  assert.equal(url.searchParams.get('after'), '42')
  const next = b.selectWorkspace('wsp_viewer')
  assert.equal(source.closed, true)
  source.callback({ data: JSON.stringify({ seq: 43 }) })
  assert.equal(events, 0)
  await next.info()
  const sub = next.subscribe({ after: 9 }, () => {})
  assert.equal(new URL(streams[1].url, 'https://studio.test').searchParams.get('workspace'), 'wsp_viewer')
  sub.close()
})

test('a pending SSE bootstrap cannot open a stream after workspace changes', async () => {
  setup()
  const b = httpBackend(); await b.info()
  let finish
  respond = () => new Promise((resolve) => { finish = resolve })
  b.subscribe({}, () => {})
  b.selectWorkspace('wsp_viewer')
  finish({ last_event_seq: 42 })
  await flush()
  assert.equal(streams.length, 0)
})

test('prepared approval cannot be confirmed after changing workspace', async () => {
  setup()
  const b = httpBackend(); await b.info()
  const review = await b.prepareDecision('approval', 'approve')
  b.selectWorkspace('wsp_viewer')
  const count = calls.length
  await assert.rejects(review.confirm(), (e) => /workspace changed/.test(e.message))
  assert.equal(calls.length, count)
})

test('changing workspace while the passkey is pending never sends the decision', async () => {
  setup()
  const b = httpBackend(); await b.info()
  const review = await b.prepareDecision('approval', 'approve')
  let finish
  navigator.credentials.get = () => new Promise((resolve) => { finish = resolve })
  respond = () => ({ challenge: 'YQ' })
  const pending = review.confirm()
  await flush()
  b.selectWorkspace('wsp_viewer')
  finish({ rawId: new ArrayBuffer(1), response: { clientDataJSON: new ArrayBuffer(1), authenticatorData: new ArrayBuffer(1), signature: new ArrayBuffer(1) } })
  await assert.rejects(pending, (e) => /workspace changed/.test(e.message))
  assert.equal(calls.some((c) => c.url.endsWith('/decide')), false)
})


test('successful approval options and decision both retain the workspace and CSRF', async () => {
  setup()
  const b = httpBackend(); await b.info()
  const review = await b.prepareDecision('approval', 'deny')
  navigator.credentials.get = async () => ({ rawId: new ArrayBuffer(1), response: { clientDataJSON: new ArrayBuffer(1), authenticatorData: new ArrayBuffer(1), signature: new ArrayBuffer(1) } })
  respond = () => ({ challenge: 'YQ' })
  await review.confirm()
  for (const call of calls.filter((c) => c.method === 'POST')) {
    assert.equal(call.headers['x-ogun-workspace'], 'wsp_editor')
    assert.equal(call.headers['x-ogun-csrf'], 'csrf')
  }
  const decision = calls.find((c) => c.url.endsWith('/decide'))
  assert.equal(JSON.parse(decision.body).decision, 'deny')
  assert.equal(JSON.parse(decision.body).request_hash, 'hash')
})

test('data and subscriptions cannot silently use an implicit workspace before discovery', async () => {
  // Calls made before discovery wait for it; with no membership they fail, and nothing went out meanwhile.
  setup({ ...me, workspaces: [] })
  const b = httpBackend()
  const early = b.status()
  let subscriptionError = null
  b.subscribe({}, () => {}, (e) => (subscriptionError = e))
  await new Promise((r) => setTimeout(r, 20))
  assert.equal(calls.length, 0)
  await b.info()
  await assert.rejects(early, (e) => e.code === 'api.permission_denied')
  await new Promise((r) => setTimeout(r, 20))
  assert.equal(subscriptionError?.code, 'api.permission_denied')
  assert.equal(calls.filter((c) => !c.url.endsWith('/me')).length, 0)
  assert.equal(streams.length, 0)
  setup({ ...me, workspaces: [] })
  const info = await httpBackend().info()
  assert.equal(info.connected, false)
  assert.equal(info.canWrite, false)
  assert.equal(info.decideChannel, null)
})
