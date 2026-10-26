// A real hub on this computer for the sign-in acceptance: ogund (profile `server`), ogun-server and an OpenID
// Connect provider that behaves like Dex for a public client (idp_mock.py). Everything listens on loopback only.
import { spawn, spawnSync } from 'node:child_process'
import { mkdirSync, writeFileSync, existsSync, rmSync, openSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { http, killPid, sleep, waitFor, started } from './lib.mjs'

const here = dirname(fileURLToPath(import.meta.url))

/** A cookie jar and a manual-redirect fetch, enough to walk the hub's and the provider's pages like a browser. */
export class Browser {
  constructor() {
    this.jar = new Map()
  }

  async get(url, init = {}) {
    return this.request(url, { ...init, method: 'GET' })
  }

  async request(url, init = {}) {
    const headers = { ...(init.headers ?? {}) }
    const host = new URL(url).host
    const cookies = [...this.jar.entries()].filter(([k]) => k.startsWith(host + '|')).map(([k, v]) => `${k.split('|')[1]}=${v}`)
    if (cookies.length) headers.cookie = cookies.join('; ')
    const r = await fetch(url, { ...init, headers, redirect: 'manual' })
    for (const c of r.headers.getSetCookie?.() ?? []) {
      const [pair, ...attrs] = c.split(';')
      const [name, ...v] = pair.split('=')
      if (/max-age=0/i.test(attrs.join(';'))) this.jar.delete(`${host}|${name.trim()}`)
      else this.jar.set(`${host}|${name.trim()}`, v.join('='))
    }
    return r
  }

  /** Follows redirects (and returns the final response) until `stop(url)` says to halt before requesting it. */
  async follow(url, stop = () => false, init = {}) {
    let r = await this.request(url, init)
    for (let i = 0; i < 12 && [301, 302, 303, 307].includes(r.status); i++) {
      const next = new URL(r.headers.get('location'), url).href
      if (stop(next)) return { response: r, stoppedAt: next }
      url = next
      r = await this.get(url)
    }
    return { response: r, stoppedAt: null }
  }
}

/** Signs a person in at the provider's own pages: the login form, then the redirect target. */
export async function loginAtProvider(browser, authorizeUrl, { login, password }, stopAt) {
  const { response } = await browser.follow(authorizeUrl)
  const page = await response.text()
  const fields = {}
  for (const m of page.matchAll(/<input type="hidden" name="([^"]+)" value="([^"]*)">/g)) {
    fields[m[1]] = m[2].replace(/&amp;/g, '&').replace(/&quot;/g, '"').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&#x27;/g, "'")
  }
  const idp = new URL(authorizeUrl).origin
  const body = new URLSearchParams({ ...fields, login, password })
  const posted = await browser.request(`${idp}/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body,
  })
  if (posted.status !== 302) throw new Error(`the provider did not accept the login (${posted.status})`)
  return new URL(posted.headers.get('location'), idp).href
}

export class Hub {
  constructor(o) {
    this.root = o.root
    this.bin = o.bin
    this.idpPort = o.idpPort ?? 38420
    this.serverPort = o.serverPort ?? 38421
    this.idleMinutes = o.idleMinutes ?? 1
    this.base = `http://127.0.0.1:${this.serverPort}`
    this.issuer = `http://127.0.0.1:${this.idpPort}`
    this.pids = []
    this.home = join(this.root, 'home')
  }

  env(extra = {}) {
    return { ...process.env, OGUN_HOME: this.home, OGUN_LOG: 'info', ...extra }
  }

  run(exe, args, input) {
    return spawnSync(join(this.bin, exe), args, { env: this.env(), input, encoding: 'utf8', timeout: 60000 })
  }

  spawnBg(exe, args, logName) {
    const out = join(this.root, logName)
    const fd = openSync(out, 'a')
    const p = spawn(join(this.bin, exe), args, { env: this.env(), stdio: ['ignore', fd, fd], windowsHide: true })
    started.add(p.pid)
    this.pids.push(p.pid)
    return p
  }

  async start() {
    if (existsSync(this.root)) rmSync(this.root, { recursive: true, force: true })
    mkdirSync(join(this.root, 'secrets'), { recursive: true })
    mkdirSync(this.home, { recursive: true })
    // The provider.
    const idp = spawn('python', [join(here, 'idp_mock.py'), '--port', String(this.idpPort)], { stdio: 'ignore', windowsHide: true })
    started.add(idp.pid)
    this.pids.push(idp.pid)
    await waitFor(async () => {
      try {
        return (await http(`${this.issuer}/.well-known/openid-configuration`)).status === 200
      } catch {
        return false
      }
    }, { what: 'the provider' })
    // The hub's own daemon and the bridge credential (a person at a terminal creates it; here the pinned CLI does).
    this.spawnBg('ogund.exe', ['--profile', 'server'], 'ogund.log')
    await waitFor(() => this.run('ogun.exe', ['--profile', 'server', 'daemon', 'status']).status === 0, { ms: 30000, what: 'the hub daemon' })
    const cred = this.run('ogun.exe', ['--profile', 'server', 'credentials', 'add', 'server', '--kind', 'bridge'], 'y\n')
    if (cred.status !== 0) throw new Error('the bridge credential was not created: ' + cred.stderr)
    writeFileSync(join(this.root, 'secrets', 'oidc-secret'), 'web-secret-for-acceptance-only')
    const cfg = {
      public_url: this.base,
      listen: `127.0.0.1:${this.serverPort}`,
      state_dir: join(this.root, 'server').replace(/\\/g, '/'),
      sessions: { idle_timeout_minutes: this.idleMinutes, absolute_timeout_hours: 12 },
      daemons: { default: { ogun_home: this.home.replace(/\\/g, '/'), profile: 'server', credential: 'server' } },
      oidc: {
        providers: [{
          id: 'sample', display_name: 'the acceptance directory', issuer: this.issuer,
          client_id: 'ogun-web', native_client_id: 'ogun-studio',
          client_secret_file: join(this.root, 'secrets', 'oidc-secret').replace(/\\/g, '/'), algorithms: ['ES256'],
        }],
      },
      remote_approvals: false,
      sync: { enabled: false },
    }
    this.config = join(this.root, 'server.json')
    writeFileSync(this.config, JSON.stringify(cfg, null, 2))
    const init = this.run('ogun-server.exe', ['--config', this.config, 'init', '--name', 'Main'])
    if (init.status !== 0) throw new Error('ogun-server init failed: ' + init.stderr)
    this.workspace = /wsp_[0-9A-Z]{26}/.exec(init.stdout)[0]
    this.ownerLink = /http:\/\/\S+\/enroll\/\S+/.exec(init.stdout)[0]
    // The server holds its store open while it runs, so the invitations are made now.
    this.viewerLink = this.invite('viewer')
    this.spawnBg('ogun-server.exe', ['--config', this.config, 'serve'], 'ogun-server.log')
    await waitFor(async () => {
      try {
        return (await http(`${this.base}/auth/providers`)).status === 200
      } catch {
        return false
      }
    }, { ms: 30000, what: 'ogun-server' })
    return this
  }

  /** Accepts an enrolment link in a "browser" signed in at the provider as `who`; returns the browser (with the hub's cookie). */
  async enroll(link, who) {
    const b = new Browser()
    const landing = await (await b.get(link)).text()
    const login = /href="(\/auth\/login\?[^"]+)"/.exec(landing)?.[1]?.replace(/&amp;/g, '&')
    if (!login) throw new Error('the enrolment page has no sign-in link: ' + landing.slice(0, 200))
    const { stoppedAt, response } = await b.follow(this.base + login, (u) => u.startsWith(this.issuer + '/auth'))
    const authorize = stoppedAt ?? response.headers.get('location')
    const callback = await loginAtProvider(b, authorize, who)
    const done = await b.get(callback)
    if (done.status !== 200) throw new Error(`the hub did not accept the sign-in (${done.status}): ${(await done.text()).slice(0, 200)}`)
    return b
  }

  invite(role) {
    const r = this.run('ogun-server.exe', ['--config', this.config, 'invite', '--workspace', this.workspace, '--role', role])
    if (r.status !== 0) throw new Error('invite failed: ' + r.stderr)
    return /http:\/\/\S+\/enroll\/\S+/.exec(r.stdout)[0]
  }

  /** The hub's ogun-server cannot be reached by invite while it runs (the store is locked): stop, invite, start. */
  async restartServer() {
    const idx = this.pids.length - 1
    killPid(this.pids[idx])
    await sleep(800)
    this.spawnBg('ogun-server.exe', ['--config', this.config, 'serve'], 'ogun-server.log')
    await waitFor(async () => {
      try {
        return (await http(`${this.base}/auth/providers`)).status === 200
      } catch {
        return false
      }
    }, { ms: 30000, what: 'ogun-server again' })
  }

  async stop() {
    for (const p of this.pids.reverse()) killPid(p)
    this.pids = []
  }
}

