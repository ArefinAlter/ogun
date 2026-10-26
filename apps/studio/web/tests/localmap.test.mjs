import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import ts from 'typescript'

// Compile the real translation table, with no browser and no extra dependency.
const dir = await mkdtemp(join(tmpdir(), 'ogun-localmap-'))
const source = await readFile(new URL('../src/api/localMap.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } })
await writeFile(join(dir, 'localMap.mjs'), outputText)
const { toDaemonCall, RENAMED, KEYED, WATCH_WAIT_MS } = await import(pathToFileURL(join(dir, 'localMap.mjs')).href)
after(() => rm(dir, { recursive: true, force: true }))

let n = 0
const key = (action) => `k-${action}-${++n}`

test('the service names the screens use become the daemon names', () => {
  const expect = {
    'agent.settings': 'agent.settings.get',
    'agent.set_settings': 'agent.settings.put',
    'agent.threads': 'agent.threads.list',
    'agent.thread': 'agent.threads.get',
    'agent.create': 'agent.threads.create',
    'agent.set_access': 'agent.access.set',
    'agent.rename': 'agent.threads.rename',
    'agent.remove': 'agent.threads.delete',
    'connections.remove': 'connections.delete',
    environments: 'environments.list',
  }
  assert.deepEqual({ ...RENAMED }, expect)
  for (const [from, to] of Object.entries(expect)) assert.equal(toDaemonCall(from, {}, key).method, to)
})

test('a name that does not differ passes through untouched', () => {
  for (const m of ['agent.models', 'agent.tools', 'agent.send', 'agent.stop', 'agent.act', 'connections.list', 'connections.test', 'runtime.get', 'ladder.get', 'storage.info', 'secrets.list', 'sources.get']) {
    const c = toDaemonCall(m, { a: 1 }, key)
    assert.equal(c.method, m)
    assert.equal(c.params.a, 1)
  }
})

test('thread watchers ask the daemon to hold the call until something changes', () => {
  const list = toDaemonCall('agent.threads', { since: 4 }, key)
  assert.equal(list.params.wait_ms, WATCH_WAIT_MS)
  assert.equal(list.params.since, 4)
  const one = toDaemonCall('agent.thread', { thread_id: 'thr_1' }, key)
  assert.equal(one.params.wait_ms, WATCH_WAIT_MS)
  assert.equal(one.params.thread_id, 'thr_1')
  // A caller that sets its own wait keeps it.
  assert.equal(toDaemonCall('agent.threads', { wait_ms: 100 }, key).params.wait_ms, 100)
})

test('connections.put is reshaped and keyed', () => {
  const input = { name: 'x', connector: 'file', auth: 'none', settings: {}, secret_refs: {} }
  const c = toDaemonCall('connections.put', { input }, key)
  assert.equal(c.method, 'connections.put')
  assert.deepEqual(c.params.connection, input)
  assert.equal('input' in c.params, false)
  assert.match(String(c.params.idempotency_key), /^k-connections-put-/)
})

test('every keyed mutation gets a fresh key, and a key the caller brought is kept', () => {
  for (const [from, extra] of [
    ['agent.set_settings', { patch: { offline: true } }],
    ['connections.remove', { name: 'x' }],
    ['runtime.set', { workflow: null, patch: { retries: 2 } }],
    ['runtime.reset', { workflow: 'w', field: 'retries' }],
    ['ladder.put', { providers: [] }],
    ['scheduler.set_enabled', { enabled: false }],
    ['schedules.add', { workflow: 'w', cron: '0 2 * * *' }],
  ]) {
    const a = toDaemonCall(from, extra, key)
    const b = toDaemonCall(from, extra, key)
    assert.equal(typeof a.params.idempotency_key, 'string', from)
    assert.notEqual(a.params.idempotency_key, b.params.idempotency_key, from)
    assert.ok(KEYED.has(a.method), from)
  }
  assert.equal(toDaemonCall('ladder.put', { providers: [], idempotency_key: 'mine' }, key).params.idempotency_key, 'mine')
  // Reads and the card buttons (which carry their own key) get none added.
  assert.equal('idempotency_key' in toDaemonCall('connections.list', {}, key).params, false)
  assert.equal('idempotency_key' in toDaemonCall('agent.act', { thread_id: 't', item_id: 'i', action: 'store' }, key).params, false)
})

test('the caller\'s parameters are never mutated', () => {
  const params = Object.freeze({ thread_id: 'thr_1' })
  assert.doesNotThrow(() => toDaemonCall('agent.thread', params, key))
  assert.deepEqual({ ...params }, { thread_id: 'thr_1' })
})
