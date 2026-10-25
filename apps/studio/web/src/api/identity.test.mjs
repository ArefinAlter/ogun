// node --test src/api/identity.test.mjs
//
// The identity shown in Studio-web and the right to write both come from `GET /api/v1/me`. These are the shapes
// ogun-server sends (crates/ogun-server/src/routes/api.rs), plus the flat shape the frontend was first written for.

import assert from 'node:assert/strict'
import { test } from 'node:test'

import { identityOf } from './identity.ts'

test('a session: the user record and the membership give the name, the role and the approver flag', () => {
  const who = identityOf({
    kind: 'session',
    user: { user_id: 'usr_1', display_name: 'demo', email: 'demo@ogun.local' },
    workspaces: [{ workspace_id: 'wsp_1', name: 'Main', role: 'owner', approver: true }],
    csrf_token: 'x',
    remote_approvals: false,
  })
  assert.deepEqual(who, { id: 'usr_1', name: 'demo', roles: ['owner'], approver: true })
})

test('a session without a display name falls back to the email, then to the id', () => {
  assert.equal(identityOf({ kind: 'session', user: { user_id: 'usr_1', email: 'a@b.c' }, workspaces: [] }).name, 'a@b.c')
  assert.equal(identityOf({ kind: 'session', user: { user_id: 'usr_1' }, workspaces: [] }).name, 'usr_1')
  assert.equal(identityOf({}).name, 'signed in')
})

test('a token acts as an agent: its ceiling is the role and it is never an approver', () => {
  const who = identityOf({
    kind: 'token',
    user: { user_id: 'usr_1', display_name: 'demo' },
    token: { role: 'editor' },
    workspaces: [{ role: 'owner', approver: true }],
  })
  assert.deepEqual(who.roles, ['editor'])
  assert.equal(who.approver, false)
})

test('a member with no membership has no role, so Studio stays read-only', () => {
  assert.deepEqual(identityOf({ kind: 'session', user: { user_id: 'usr_1', display_name: 'x' }, workspaces: [] }).roles, [])
})

test('the flat shape still works', () => {
  const who = identityOf({ user_id: 'usr_2', name: 'Ada', roles: ['viewer'], approver: false })
  assert.deepEqual(who, { id: 'usr_2', name: 'Ada', roles: ['viewer'], approver: false })
})
