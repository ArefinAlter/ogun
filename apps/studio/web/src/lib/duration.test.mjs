// node --test src/lib/duration.test.mjs

import assert from 'node:assert/strict'
import { test } from 'node:test'

import { fmtDuration } from './duration.ts'

test('a duration reads as milliseconds, seconds or minutes, and never as a date', () => {
  assert.equal(fmtDuration(420), '420 ms')
  assert.equal(fmtDuration(1234), '1.2 s')
  assert.equal(fmtDuration(12_345), '12 s')
  assert.equal(fmtDuration(185_000), '3 min 5 s')
  assert.equal(fmtDuration(undefined), '–')
  assert.equal(fmtDuration(Number.NaN), '–')
})
