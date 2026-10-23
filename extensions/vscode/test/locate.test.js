'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');
const { locate, locateNearest } = require('../src/locate');

const doc = `{
  "workflow": "w",
  "nodes": [
    { "id": "a", "op": "hash@1" },
    { "id": "b/c~d", "inputs": ["a.artifacts", "x.y"], "config": { "url": "https://e.org/?q=\\"1\\"" } }
  ]
}`;

const at = (p) => { const r = locate(doc, p); return r && doc.slice(r.start, r.end); };

test('locates values by JSON Pointer', () => {
  assert.strictEqual(at('/workflow'), '"w"');
  assert.strictEqual(at('/nodes/0/op'), '"hash@1"');
  assert.strictEqual(at('/nodes/1/inputs/1'), '"x.y"');
  assert.strictEqual(at('/nodes/1/id'), '"b/c~d"');
  assert.ok(at('/nodes/1/config').startsWith('{ "url"'));
  assert.strictEqual(at('/nodes/1/config/url'), '"https://e.org/?q=\\"1\\""');
  assert.strictEqual(at(''), doc);
});

test('missing members fall back to the nearest existing parent', () => {
  assert.strictEqual(locate(doc, '/nodes/0/policy'), null);
  const r = locateNearest(doc, '/nodes/0/policy/retries');
  assert.strictEqual(doc.slice(r.start, r.end), '{ "id": "a", "op": "hash@1" }');
  assert.strictEqual(locateNearest('{', '/x').start, 0, 'broken JSON falls back to the document start');
});

test('the bundled schema is the spec schema', () => {
  const spec = fs.readFileSync(path.join(__dirname, '../../../docs/spec/schema/workflow-ir.v0.schema.json'), 'utf8');
  const bundled = fs.readFileSync(path.join(__dirname, '../schemas/workflow-ir.v0.schema.json'), 'utf8');
  assert.strictEqual(bundled, spec);
});
