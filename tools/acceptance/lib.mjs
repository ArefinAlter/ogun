// Helpers for the standalone acceptance run: start the real Ogun Studio on a scratch profile, drive its window
// through the WebView2 remote-debugging port (Playwright over CDP), call the engine through the app's own
// commands (the same path the screens use: webview -> shell -> named pipe -> ogund), and write evidence.
//
// Nothing here reads or writes the real user's app data: every run sets OGUN_HOME, OGUN_STUDIO_DATA and
// WEBVIEW2_USER_DATA_FOLDER to folders under the scratch root.

import { createRequire } from 'node:module'
import { spawn, execFileSync } from 'node:child_process'
import { mkdirSync, writeFileSync, existsSync, rmSync, readFileSync, appendFileSync } from 'node:fs'
import { join } from 'node:path'

const require = createRequire(process.env.OGUN_PLAYWRIGHT_FROM ?? 'B:/dynodoc/frontend/researcher/package.json')
const { chromium } = require('@playwright/test')

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

export function ps(command) {
  return execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', command], { encoding: 'utf8' }).trim()
}

/** Children of a process: [{pid, name, cmd}]. */
export function children(pid) {
  const out = ps(
    `Get-CimInstance Win32_Process -Filter "ParentProcessId=${pid}" | Select-Object ProcessId,Name,CommandLine | ConvertTo-Json -Compress`,
  )
  if (!out) return []
  const v = JSON.parse(out)
  return (Array.isArray(v) ? v : [v]).map((p) => ({ pid: p.ProcessId, name: p.Name, cmd: p.CommandLine ?? '' }))
}

export function alive(pid) {
  return ps(`if (Get-Process -Id ${pid} -ErrorAction SilentlyContinue) { 'yes' } else { 'no' }`) === 'yes'
}

export function killPid(pid) {
  try {
    execFileSync('taskkill.exe', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore' })
  } catch {
    /* already gone */
  }
}

export class Evidence {
  constructor(root, scenario) {
    this.dir = join(root, scenario)
    mkdirSync(this.dir, { recursive: true })
    this.log = join(this.dir, 'log.txt')
    this.results = []
  }

  note(text) {
    const line = `[${new Date().toISOString()}] ${text}`
    appendFileSync(this.log, line + '\n')
    console.log(line)
  }

  save(name, value) {
    writeFileSync(join(this.dir, name), typeof value === 'string' ? value : JSON.stringify(value, null, 2))
  }

  check(name, ok, detail = '') {
    this.results.push({ name, ok: !!ok, detail: String(detail).slice(0, 600) })
    this.note(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ' :: ' + String(detail).slice(0, 300) : ''}`)
    return !!ok
  }

  finish() {
    this.save('results.json', this.results)
    const failed = this.results.filter((r) => !r.ok).length
    this.note(`${this.results.length - failed}/${this.results.length} checks passed`)
    return failed === 0
  }
}

export class App {
  /** @param {{root: string, exe: string, port?: number, env?: Record<string,string>}} o */
  constructor(o) {
    this.root = o.root
    this.exe = o.exe
    this.port = o.port ?? 9223
    this.extraEnv = o.env ?? {}
    this.proc = null
    this.browser = null
    this.page = null
  }

  env() {
    const r = this.root
    for (const d of ['home', 'shell', 'webview']) mkdirSync(join(r, d), { recursive: true })
    const env = { ...process.env }
    delete env.ELECTRON_RUN_AS_NODE
    Object.assign(env, {
      OGUN_HOME: join(r, 'home'),
      OGUN_STUDIO_DATA: join(r, 'shell'),
      WEBVIEW2_USER_DATA_FOLDER: join(r, 'webview'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${this.port}`,
      OGUN_BENCH_STATE_DIR: join(r, 'bench'),
      ...this.extraEnv,
    })
    return env
  }

  async start() {
    this.proc = spawn(this.exe, [], { env: this.env(), stdio: ['ignore', 'pipe', 'pipe'], windowsHide: false })
    const out = []
    this.proc.stdout.on('data', (d) => out.push(String(d)))
    this.proc.stderr.on('data', (d) => out.push(String(d)))
    this.output = out
    const deadline = Date.now() + 60000
    for (;;) {
      try {
        this.browser = await chromium.connectOverCDP(`http://127.0.0.1:${this.port}`)
        const ctx = this.browser.contexts()[0]
        this.page = ctx?.pages()[0]
        if (this.page) break
      } catch {
        /* not up yet */
      }
      if (Date.now() > deadline) throw new Error('the app window did not appear: ' + out.join('').slice(-500))
      await sleep(500)
    }
    await this.page.waitForLoadState('domcontentloaded')
    return this
  }

  async stop(kill = false) {
    try {
      await this.browser?.close()
    } catch {
      /* ignore */
    }
    if (this.proc?.pid) {
      if (kill) killPid(this.proc.pid)
      else {
        // The window's close button path: ask the shell to exit and wait for the engine to go with it.
        try {
          await this.page?.evaluate(() => window.close())
        } catch {
          /* ignore */
        }
        await sleep(1500)
        if (alive(this.proc.pid)) killPid(this.proc.pid)
      }
    }
  }

  get pid() {
    return this.proc.pid
  }

  /** `ogund` children of the shell. */
  engines() {
    return children(this.pid).filter((c) => /ogund/i.test(c.name))
  }

  async invoke(cmd, args = {}) {
    const r = await this.page.evaluate(
      async ([c, a]) => {
        try {
          return { ok: await window.__TAURI_INTERNALS__.invoke(c, a) }
        } catch (e) {
          return { err: e }
        }
      },
      [cmd, args],
    )
    if (r.err) {
      const e = new Error(typeof r.err === 'string' ? r.err : (r.err.message ?? JSON.stringify(r.err)))
      e.envelope = r.err
      throw e
    }
    return r.ok
  }

  /** A daemon method through the shell's `engine_call` (the closed relay list). */
  engine(method, params = {}) {
    return this.invoke('engine_call', { method, params })
  }

  async waitEngine(ms = 60000) {
    const end = Date.now() + ms
    let last
    while (Date.now() < end) {
      try {
        last = await this.invoke('engine_info')
        if (last.state === 'running' || last.state === 'external') return last
      } catch (e) {
        last = { error: String(e) }
      }
      await sleep(500)
    }
    throw new Error('the engine did not become ready: ' + JSON.stringify(last))
  }

  async shot(path) {
    await this.page.screenshot({ path })
  }

  async goto(hash) {
    await this.page.evaluate((h) => {
      window.location.hash = h
    }, hash)
    await sleep(400)
  }
}

export function readJson(path) {
  return JSON.parse(readFileSync(path, 'utf8'))
}

export function cleanDir(path) {
  if (existsSync(path)) rmSync(path, { recursive: true, force: true })
  mkdirSync(path, { recursive: true })
}

export async function waitFor(fn, { ms = 30000, every = 400, what = 'condition' } = {}) {
  const end = Date.now() + ms
  let last
  while (Date.now() < end) {
    last = await fn()
    if (last) return last
    await sleep(every)
  }
  throw new Error(`timed out waiting for ${what}`)
}

export async function http(url, opts = {}) {
  const r = await fetch(url, opts)
  const text = await r.text()
  let json
  try {
    json = JSON.parse(text)
  } catch {
    /* not json */
  }
  return { status: r.status, text, json, headers: r.headers }
}
