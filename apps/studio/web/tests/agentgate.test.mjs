import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import ts from 'typescript'

const dir = await mkdtemp(join(tmpdir(), 'ogun-gate-'))
const source = await readFile(new URL('../src/lib/agentGate.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } })
await writeFile(join(dir, 'agentGate.mjs'), outputText)
const { agentReadiness } = await import(pathToFileURL(join(dir, 'agentGate.mjs')).href)
after(() => rm(dir, { recursive: true, force: true }))

const settings = { offline: false, consented_providers: ['claude'], spend_cap_usd: 2, day_cap_usd: 10 }
const claude = { name: 'claude', kind: 'anthropic', enabled: true, model: 'claude-sonnet-5-5', api_key_secret: 'claude_api_key', capabilities: ['chat', 'json_schema'] }

test('ready only when a provider has a model, a stored key and consent', () => {
  const r = agentReadiness({ settings, providers: [claude], secretNames: ['claude_api_key'] })
  assert.equal(r.ready, true)
  assert.equal(r.provider, 'claude')
})

test('each missing piece is named, in the order a person fixes them', () => {
  const ok = { settings, providers: [claude], secretNames: ['claude_api_key'] }
  assert.equal(agentReadiness({ ...ok, settings: { ...settings, offline: true } }).reason, 'offline')
  assert.equal(agentReadiness({ ...ok, settings: { ...settings, spend_cap_usd: 0 } }).reason, 'no_cap')
  assert.equal(agentReadiness({ ...ok, settings: { ...settings, day_cap_usd: 0 } }).reason, 'no_cap')
  assert.equal(agentReadiness({ ...ok, providers: [] }).reason, 'no_provider')
  assert.equal(agentReadiness({ ...ok, providers: [{ ...claude, enabled: false }] }).reason, 'no_provider')
  assert.equal(agentReadiness({ ...ok, providers: [{ ...claude, model: '' }] }).reason, 'no_provider')
  assert.equal(agentReadiness({ ...ok, secretNames: [] }).reason, 'no_key')
  assert.equal(agentReadiness({ ...ok, settings: { ...settings, consented_providers: [] } }).reason, 'no_consent')
  for (const reason of ['offline', 'no_cap', 'no_provider', 'no_key', 'no_consent']) assert.ok(reason)
})

test('a keyless local provider needs no key but still needs consent', () => {
  const ollama = { name: 'ollama', kind: 'ollama', enabled: true, model: 'qwen3:8b', capabilities: ['chat'] }
  assert.equal(agentReadiness({ settings, providers: [ollama], secretNames: [] }).reason, 'no_consent')
  assert.equal(agentReadiness({ settings: { ...settings, consented_providers: ['ollama'] }, providers: [ollama], secretNames: [] }).ready, true)
})

test('a hosted kind without a key name is never ready, even with consent', () => {
  const bare = { ...claude, api_key_secret: undefined }
  assert.equal(agentReadiness({ settings, providers: [bare], secretNames: ['claude_api_key'] }).reason, 'no_key')
})

test('an unknown state is left to the daemon', () => {
  assert.equal(agentReadiness({ settings: null, providers: [claude], secretNames: [] }).ready, true)
  assert.equal(agentReadiness({ settings, providers: null, secretNames: [] }).ready, true)
})

test('the messages point at Settings, Providers', () => {
  const r = agentReadiness({ settings, providers: [], secretNames: [] })
  assert.match(r.message, /Settings, Providers/)
})
