// node --test src/lib/agentTools.test.mjs
//
// The agent's tool table is a security boundary: these tests state what it must never contain and how access gates it.

import assert from 'node:assert/strict'
import { test } from 'node:test'

import { ACCESS_ORDER, DEFAULT_INSTRUCTIONS, TOOLS, mayCall } from './agentTools.ts'

const all = TOOLS.map((t) => t.name)

test('no tool decides an approval, reads a secret value, runs a shell or fetches a host', () => {
  for (const name of all) {
    assert.doesNotMatch(name, /decide|approve|deny|secret|shell|exec|fetch|sudo/i, name)
  }
  // approvals are only ever listed
  assert.deepEqual(all.filter((n) => n.startsWith('approvals.')), ['approvals.list'])
})

test('tool names are unique and each names an access level that exists', () => {
  assert.equal(new Set(all).size, all.length)
  for (const t of TOOLS) assert.ok(ACCESS_ORDER.includes(t.access), t.name)
})

test('read only may validate and plan but not store or start', () => {
  assert.equal(mayCall('workflows.validate', 'read', all).ok, true)
  assert.equal(mayCall('backfills.plan', 'read', all).ok, true)
  assert.equal(mayCall('workflows.put', 'read', all).ok, false)
  assert.equal(mayCall('runs.start', 'read', all).ok, false)
})

test('author may store but not start; operate may do both', () => {
  assert.equal(mayCall('workflows.put', 'author', all).ok, true)
  assert.equal(mayCall('runs.start', 'author', all).ok, false)
  assert.equal(mayCall('runs.start', 'operate', all).ok, true)
  assert.equal(mayCall('runs.cancel', 'operate', all).ok, true)
})

test('a tool switched off in settings is refused at any level, and an unknown tool is refused', () => {
  const some = all.filter((n) => n !== 'runs.start')
  const r = mayCall('runs.start', 'operate', some)
  assert.equal(r.ok, false)
  assert.match(r.reason, /switched off/)
  assert.equal(mayCall('approvals.decide', 'operate', [...all, 'approvals.decide']).ok, false)
})

test('the default instructions say nothing that asks for a secret', () => {
  assert.doesNotMatch(DEFAULT_INSTRUCTIONS, /password|api[_ -]?key\s*[:=]|ogt1_/i)
})
