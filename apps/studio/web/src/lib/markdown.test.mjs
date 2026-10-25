// node --test src/lib/markdown.test.mjs
//
// An agent's message is untrusted text. These pin what the reader may produce from it: a tree of plain nodes, no HTML,
// and links only to Studio's own pages or to https addresses without credentials.

import assert from 'node:assert/strict'
import { test } from 'node:test'

import { parseInline, parseMarkdown, safeHref } from './markdown.ts'

const flat = (inl) => inl.map((n) => (n.t === 'text' || n.t === 'code' ? n.v : flat(n.c))).join('')

test('raw HTML stays text and is never a node', () => {
  const [p] = parseMarkdown('<img src=x onerror=alert(1)> and <script>alert(2)</script>')
  assert.equal(p.t, 'p')
  assert.ok(p.c.every((n) => n.t === 'text'))
  assert.equal(flat(p.c), '<img src=x onerror=alert(1)> and <script>alert(2)</script>')
})

test('only Studio pages and credential-free https addresses become links', () => {
  assert.deepEqual(safeHref('#/runs/orun_1'), { href: '#/runs/orun_1', internal: true })
  assert.equal(safeHref('https://example.org/a?b=c')?.internal, false)
  for (const bad of ['javascript:alert(1)', 'data:text/html,x', 'http://example.org', '//example.org', '/runs', 'https://user:pw@example.org/', 'https://a b.example/', '#/a b', 'mailto:a@b.c', 'JaVaScRiPt:1', '']) {
    assert.equal(safeHref(bad), null, bad)
  }
})

test('a link with an unsafe address is shown as text with its address, not as a link', () => {
  const nodes = parseInline('[click](javascript:alert(1))')
  assert.ok(nodes.every((n) => n.t === 'text'))
  assert.match(flat(nodes), /click/)
})

test('inline code, strong and emphasis nest and do not leak across code', () => {
  const nodes = parseInline('use `a*b*c` and **bold *and em***')
  assert.equal(nodes[1].t, 'code')
  assert.equal(nodes[1].v, 'a*b*c')
  assert.equal(nodes[3].t, 'strong')
})

test('lists, ordered lists, headings, quotes and rules', () => {
  const blocks = parseMarkdown('# One\n\n- a\n- b\n  continued\n\n1. x\n2. y\n\n> quoted\n\n---\n')
  assert.deepEqual(blocks.map((b) => b.t), ['h', 'ul', 'ol', 'quote', 'hr'])
  assert.equal(blocks[1].items.length, 2)
  assert.equal(flat(blocks[1].items[1]), 'b continued')
})

test('a table has a head and rows, and a pipe inside code stays in its cell', () => {
  const [t] = parseMarkdown('| name | op |\n|---|---|\n| `a|b` | hash@1 |\n| c | d |')
  assert.equal(t.t, 'table')
  assert.equal(t.head.length, 2)
  assert.equal(t.rows.length, 2)
  assert.equal(flat(t.rows[0][0]), 'a|b')
})

test('a fenced block keeps its text exactly, and an unclosed one ends with the text (streaming)', () => {
  const [a] = parseMarkdown('```json\n{"a": 1}\n```')
  assert.deepEqual(a, { t: 'code', lang: 'json', v: '{"a": 1}' })
  const [b] = parseMarkdown('```\nunfinished')
  assert.equal(b.t, 'code')
  assert.equal(b.v, 'unfinished')
})

test('a backslash escapes punctuation', () => {
  assert.equal(flat(parseInline('1 \\* 2 \\* 3')), '1 * 2 * 3')
})

test('it never throws on odd input', () => {
  for (const s of ['', '**', '[', '[](', '`', '| a |\n|--', '```', '> ', '1.', '\\', '*a', '_a', '[a](b', '#', '####### x']) {
    assert.doesNotThrow(() => parseMarkdown(s), s)
  }
})
