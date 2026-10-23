'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { encode, FrameParser, LspClient, findOnPath, resolveServerCommand } = require('../src/lsp');

// The fake server runs from a string (`node -e`), so the test runner never mistakes it for a test.
const FAKE_SERVER = `const LSP_MODULE = ${JSON.stringify(path.join(__dirname, '..', 'src', 'lsp.js'))};
// A tiny language server for the client tests: answers initialize, echoes, asks the client one
// question, publishes diagnostics on didOpen, and leaves on shutdown and exit.
const { encode, FrameParser } = require(LSP_MODULE);

const parser = new FrameParser();
const send = (m) => process.stdout.write(encode(m));
let configAnswer = null;

process.stdin.on('data', (chunk) => {
  for (const msg of parser.push(chunk)) {
    if (msg.method === 'initialize') {
      send({ jsonrpc: '2.0', id: msg.id, result: { capabilities: { hoverProvider: true, positionEncoding: 'utf-16' } } });
    } else if (msg.method === 'initialized') {
      send({ jsonrpc: '2.0', id: 'srv-1', method: 'workspace/configuration', params: { items: [{ section: 'ogun' }, { section: 'x' }] } });
    } else if (msg.id === 'srv-1') {
      configAnswer = msg.result;
    } else if (msg.method === 'test/echo') {
      send({ jsonrpc: '2.0', id: msg.id, result: { echo: msg.params, configAnswer } });
    } else if (msg.method === 'test/fail') {
      send({ jsonrpc: '2.0', id: msg.id, error: { code: -32602, message: 'invalid parameters for test/fail' } });
    } else if (msg.method === 'textDocument/didOpen') {
      send({ jsonrpc: '2.0', method: 'textDocument/publishDiagnostics', params: { uri: msg.params.textDocument.uri, diagnostics: [{ range: { start: { line: 0, character: 0 }, end: { line: 0, character: 1 } }, message: 'é ok', severity: 1 }] } });
    } else if (msg.method === 'shutdown') {
      send({ jsonrpc: '2.0', id: msg.id, result: null });
    } else if (msg.method === 'exit') {
      process.exit(0);
    }
  }
});
`;

test('frames round trip, split across chunks and joined in one', () => {
  const a = encode({ jsonrpc: '2.0', id: 1, result: { text: 'é😀' } });
  const b = encode({ jsonrpc: '2.0', method: 'n', params: {} });
  const all = Buffer.concat([a, b]);
  const p = new FrameParser();
  const got = [];
  for (let i = 0; i < all.length; i += 7) got.push(...p.push(all.slice(i, i + 7)));
  assert.deepStrictEqual(got.map((m) => m.id || m.method), [1, 'n']);
  assert.strictEqual(got[0].result.text, 'é😀');
  assert.strictEqual(new FrameParser().push(all).length, 2);
  assert.throws(() => new FrameParser().push(Buffer.from('Content-Type: x\r\n\r\n{}')));
});

test('client talks to a server child process', async () => {
  const client = new LspClient(process.execPath, ['-e', FAKE_SERVER]);
  const caps = await client.initialize({ capabilities: {} });
  assert.strictEqual(caps.hoverProvider, true);
  const diagnostics = new Promise((resolve) => client.on('notification', (m, p) => { if (m === 'textDocument/publishDiagnostics') resolve(p); }));
  client.notify('textDocument/didOpen', { textDocument: { uri: 'file:///a.workflow.json', languageId: 'json', version: 1, text: '{}' } });
  const d = await diagnostics;
  assert.strictEqual(d.diagnostics[0].message, 'é ok');
  // Give the server's own request a moment to be answered.
  await new Promise((r) => setTimeout(r, 50));
  const echo = await client.request('test/echo', { x: 1 });
  assert.deepStrictEqual(echo.echo, { x: 1 });
  assert.deepStrictEqual(echo.configAnswer, [null, null]);
  await assert.rejects(client.request('test/fail', {}), /invalid parameters/);
  await client.stop();
  assert.ok(client.exited);
  await assert.rejects(client.request('test/echo', {}), /not running/);
});

test('a server that cannot start rejects initialize', async () => {
  const client = new LspClient(path.join(os.tmpdir(), 'no-such-ogun-lsp-binary'), []);
  await assert.rejects(client.initialize({ capabilities: {} }, 2000));
});

test('finds the server on PATH, then falls back to `ogun lsp`, else the CLI validator', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ogun-vscode-'));
  const write = (name) => {
    const p = path.join(dir, name);
    fs.writeFileSync(p, '#!/bin/sh\n');
    fs.chmodSync(p, 0o755);
    return p;
  };
  const env = { PATH: dir };
  assert.strictEqual(resolveServerCommand({ mode: 'auto' }, env, 'linux'), null);
  const ogun = write('ogun');
  assert.deepStrictEqual(resolveServerCommand({ mode: 'auto' }, env, 'linux'), { command: ogun, args: ['lsp'], source: 'ogun lsp' });
  const lsp = write('ogun-lsp');
  assert.strictEqual(findOnPath('ogun-lsp', env, 'linux'), lsp);
  assert.deepStrictEqual(resolveServerCommand({ mode: 'auto' }, env, 'linux'), { command: lsp, args: [], source: 'PATH' });
  assert.strictEqual(resolveServerCommand({ mode: 'off' }, env, 'linux'), null);
  assert.strictEqual(resolveServerCommand({ mode: 'on', lspPath: '/opt/x' }, env, 'linux').command, '/opt/x');
  fs.rmSync(dir, { recursive: true, force: true });
});

// Runs against the real server when OGUN_LSP_BIN names a built ogun-lsp binary.
test('the real ogun-lsp answers diagnostics, hover and completion', { skip: !process.env.OGUN_LSP_BIN }, async () => {
  const client = new LspClient(process.env.OGUN_LSP_BIN, []);
  await client.initialize({ capabilities: {} });
  const uri = 'file:///tmp/t.workflow.json';
  const text = '{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"},{"id":"b","op":"manifest@1","inputs":["a."]}]}';
  const diags = new Promise((resolve) => client.on('notification', (m, p) => { if (m === 'textDocument/publishDiagnostics') resolve(p); }));
  client.notify('textDocument/didOpen', { textDocument: { uri, languageId: 'json', version: 1, text } });
  const d = await diags;
  assert.ok(d.diagnostics.length >= 1);
  const at = text.indexOf('"a."') + 3;
  const items = await client.request('textDocument/completion', { textDocument: { uri }, position: { line: 0, character: at } });
  assert.ok(items.some((i) => i.label === 'a.artifacts'));
  const hover = await client.request('textDocument/hover', { textDocument: { uri }, position: { line: 0, character: text.indexOf('hash@1') + 1 } });
  assert.match(hover.contents.value, /artifacts/);
  const hash = await client.request('ogun/workflowHash', { textDocument: { uri } });
  assert.strictEqual(hash.workflow_hash, null);
  await client.stop();
});
