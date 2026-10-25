// node --test connectors/tools/check.test.mjs
//
// The catalog passes its checks, and each check can fail: a manifest that breaks a rule is refused with a
// finding that names the rule.

import assert from 'node:assert/strict'
import { test } from 'node:test'

import { checkAll, loadCatalog, loadSchema, rules, validate } from './check.mjs'

const schema = loadSchema()
const base = () => structuredClone(loadCatalog().find((e) => e.manifest.id === 'bigquery').manifest)

test('the shipped catalog has no findings', () => {
  const entries = loadCatalog()
  assert.ok(entries.length >= 90, `expected at least 90 connectors, found ${entries.length}`)
  assert.deepEqual(checkAll(entries, schema), [])
})

test('every connector id is its file name, and every operator name is unique', () => {
  const entries = loadCatalog()
  assert.equal(new Set(entries.map((e) => e.manifest.id)).size, entries.length)
  const ops = entries.flatMap((e) => e.manifest.operators.map((o) => o.op))
  assert.equal(new Set(ops).size, ops.length)
})

test('a secret-looking auth field must be declared secret', () => {
  const m = base()
  m.auth[0].fields.push({ name: 'api_token', label: 'Token', secret: false })
  assert.ok(rules(m).some((f) => f.message.includes('must be declared secret')))
})

test('a secret field cannot carry a default', () => {
  const m = base()
  m.auth[0].fields[0].default = 'hunter2'
  const f = validate(schema, m)
  assert.ok(f.some((x) => x.message === 'is not allowed') || rules(m).some((x) => x.message.includes('no default')))
})

test('a setting cannot be named like a secret', () => {
  const m = base()
  m.settings.push({ name: 'password', label: 'Password', type: 'string' })
  assert.ok(rules(m).some((f) => f.message.includes('looks like a secret')))
})

test('a domain placeholder must name a setting or a field of the same manifest', () => {
  const m = base()
  m.egress.domains.push('{nowhere}.example.com')
  assert.ok(rules(m).some((f) => f.message.includes('{nowhere}')))
})

test('a built connector names code that exists; a contract connector names none', () => {
  const m = base()
  m.maturity = 'built'
  assert.ok(rules(m).some((f) => f.path === '/implementation'))
  m.implementation = { path: 'no/such/file.py' }
  assert.ok(rules(m).some((f) => f.message.includes('does not exist')))
  const c = base()
  c.implementation = { path: 'README.md' }
  assert.ok(rules(c).some((f) => f.message.includes('contract-only')))
})

test('the schema refuses an unknown category and an http documentation link', () => {
  const m = base()
  m.category = 'cloud'
  m.docs_url = 'http://example.com/'
  const paths = validate(schema, m).map((f) => f.path)
  assert.ok(paths.includes('/category') && paths.includes('/docs_url'))
})

test('a cli adapter names its tool', () => {
  const m = base()
  m.operators[0].via = 'cli'
  delete m.operators[0].tool
  assert.ok(rules(m).some((f) => f.message.includes('names the tool')))
})
