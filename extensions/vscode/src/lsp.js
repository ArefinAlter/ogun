'use strict';
// A small Language Server Protocol client in plain JavaScript: Content-Length framed JSON-RPC
// 2.0 over a child process's stdio. It has no dependency on VS Code, so `node --test` covers it;
// extension.js adapts its results to the VS Code API. It speaks to `ogun-lsp` (or `ogun lsp`),
// which runs the real validator (ADR 0011 decision 1).

const { spawn } = require('child_process');
const { EventEmitter } = require('events');
const fs = require('fs');
const path = require('path');

const HEADER_END = Buffer.from('\r\n\r\n', 'ascii');
const MAX_MESSAGE_BYTES = 64 * 1024 * 1024;

function encode(message) {
  const body = Buffer.from(JSON.stringify(message), 'utf8');
  return Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, 'ascii'), body]);
}

// Splits a byte stream into messages. `push` returns the parsed messages; malformed headers or
// bodies throw, because the stream cannot be resynchronised.
class FrameParser {
  constructor() {
    this.buffer = Buffer.alloc(0);
  }

  push(chunk) {
    this.buffer = this.buffer.length ? Buffer.concat([this.buffer, chunk]) : chunk;
    const out = [];
    for (;;) {
      const end = this.buffer.indexOf(HEADER_END);
      if (end < 0) break;
      const headers = this.buffer.slice(0, end).toString('ascii').split('\r\n');
      let length = -1;
      for (const h of headers) {
        const m = /^content-length:\s*(\d+)\s*$/i.exec(h);
        if (m) length = Number(m[1]);
      }
      if (length < 0 || length > MAX_MESSAGE_BYTES) throw new Error('bad Content-Length header');
      const start = end + HEADER_END.length;
      if (this.buffer.length < start + length) break;
      const body = this.buffer.slice(start, start + length).toString('utf8');
      this.buffer = this.buffer.slice(start + length);
      out.push(JSON.parse(body));
    }
    return out;
  }
}

// Answers the requests a server may send to its client. Ogun's server sends none today; these
// keep a generic server happy without granting it anything.
function defaultServerRequest(method, params) {
  switch (method) {
    case 'workspace/configuration':
      return (params && Array.isArray(params.items) ? params.items : []).map(() => null);
    case 'client/registerCapability':
    case 'client/unregisterCapability':
    case 'window/workDoneProgress/create':
    case 'window/showMessageRequest':
      return null;
    default:
      return undefined;
  }
}

class LspClient extends EventEmitter {
  // `command` and `args` start the server; `options.cwd` and `options.env` go to spawn.
  constructor(command, args, options = {}) {
    super();
    this.command = command;
    this.args = args || [];
    this.options = options;
    this.nextId = 1;
    this.pending = new Map();
    this.child = null;
    this.exited = false;
    this.capabilities = null;
  }

  start() {
    const parser = new FrameParser();
    this.child = spawn(this.command, this.args, {
      cwd: this.options.cwd,
      env: this.options.env || process.env,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });
    this.child.on('error', (err) => this._fail(err));
    this.child.on('exit', (code, signal) => {
      this.exited = true;
      this._fail(new Error(`language server exited (${signal || code})`));
      this.emit('exit', code, signal);
    });
    this.child.stdout.on('data', (chunk) => {
      let messages;
      try {
        messages = parser.push(chunk);
      } catch (e) {
        this._fail(e);
        this.child.kill();
        return;
      }
      for (const m of messages) this._onMessage(m);
    });
    this.child.stderr.on('data', (chunk) => this.emit('stderr', chunk.toString('utf8')));
    this.child.stdin.on('error', (err) => this._fail(err));
  }

  _fail(err) {
    for (const { reject, timer } of this.pending.values()) {
      clearTimeout(timer);
      reject(err);
    }
    this.pending.clear();
  }

  _send(message) {
    if (!this.child || this.exited || !this.child.stdin.writable) return false;
    this.child.stdin.write(encode(message));
    return true;
  }

  _onMessage(msg) {
    const isResponse = msg.id !== undefined && msg.method === undefined;
    if (isResponse) {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      clearTimeout(p.timer);
      if (msg.error) {
        const err = new Error(msg.error.message || 'language server error');
        err.code = msg.error.code;
        p.reject(err);
      } else {
        p.resolve(msg.result === undefined ? null : msg.result);
      }
      return;
    }
    if (msg.id !== undefined) {
      const result = defaultServerRequest(msg.method, msg.params);
      if (result === undefined) {
        this._send({ jsonrpc: '2.0', id: msg.id, error: { code: -32601, message: 'method not supported by this client' } });
      } else {
        this._send({ jsonrpc: '2.0', id: msg.id, result });
      }
      return;
    }
    this.emit('notification', msg.method, msg.params);
  }

  request(method, params, timeoutMs = 10000) {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      const timer = setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`${method} timed out`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      if (!this._send({ jsonrpc: '2.0', id, method, params })) {
        this.pending.delete(id);
        clearTimeout(timer);
        reject(new Error('language server is not running'));
      }
    });
  }

  notify(method, params) {
    return this._send({ jsonrpc: '2.0', method, params });
  }

  // Starts the server and runs the initialize handshake. Resolves to the server capabilities.
  async initialize(params, timeoutMs = 10000) {
    if (!this.child) this.start();
    const result = await this.request('initialize', params, timeoutMs);
    this.capabilities = (result && result.capabilities) || {};
    this.notify('initialized', {});
    return this.capabilities;
  }

  // Polite shutdown, then exit; kills the process if it does not leave.
  async stop(timeoutMs = 2000) {
    if (!this.child || this.exited) return;
    try {
      await this.request('shutdown', null, timeoutMs);
      this.notify('exit', null);
    } catch (_) {
      // fall through to kill
    }
    await new Promise((resolve) => {
      if (this.exited) return resolve();
      const timer = setTimeout(() => {
        this.child.kill();
        resolve();
      }, timeoutMs);
      this.child.once('exit', () => {
        clearTimeout(timer);
        resolve();
      });
    });
  }
}

// Finds an executable on PATH. On Windows, PATHEXT extensions are tried as well.
function findOnPath(name, env = process.env, platform = process.platform) {
  const dirs = (env.PATH || env.Path || '').split(platform === 'win32' ? ';' : ':').filter(Boolean);
  const exts = platform === 'win32' ? ['', ...(env.PATHEXT || '.EXE;.CMD;.BAT').split(';').filter(Boolean)] : [''];
  for (const dir of dirs) {
    for (const ext of exts) {
      const candidate = path.join(dir, name + ext);
      try {
        if (!fs.statSync(candidate).isFile()) continue;
        if (platform !== 'win32') fs.accessSync(candidate, fs.constants.X_OK);
        return candidate;
      } catch (_) {
        // not here
      }
    }
  }
  return null;
}

// Decides how to start the language server, or returns null to use the built-in validator.
//   settings.mode      'auto' (default), 'on' or 'off'
//   settings.lspPath   explicit server command (ogun.languageServer.path)
//   settings.ogunPath  the ogun CLI (ogun.path); `ogun lsp` is the fallback server
function resolveServerCommand(settings, env = process.env, platform = process.platform) {
  const mode = settings.mode || 'auto';
  if (mode === 'off') return null;
  if (settings.lspPath) return { command: settings.lspPath, args: [], source: 'ogun.languageServer.path' };
  const lsp = findOnPath('ogun-lsp', env, platform);
  if (lsp) return { command: lsp, args: [], source: 'PATH' };
  const ogun = settings.ogunPath || 'ogun';
  const hasDir = ogun.includes('/') || ogun.includes('\\');
  const resolved = hasDir ? (fs.existsSync(ogun) ? ogun : null) : findOnPath(ogun, env, platform);
  if (resolved) return { command: resolved, args: ['lsp'], source: 'ogun lsp' };
  return null;
}

module.exports = { encode, FrameParser, LspClient, findOnPath, resolveServerCommand, defaultServerRequest };
