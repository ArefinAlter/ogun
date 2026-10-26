// The standalone acceptance run for Ogun Studio 0.0.1b. Each scenario starts (or reuses) the real desktop app on a
// scratch profile, drives it, and writes evidence under <out>/<scenario>/.
//
//   node acceptance.mjs <scenario> [--exe path] [--root path] [--out path] [--fresh]
//
// Scenarios: s1 (install, launch, empty workspace, sample), s2 (core operators, backfill, kills), s3 (schedules
// and streams), s4 (approvals), s5 (code operators and the sandbox), s6 (agent threads), s7 (hub sign-in),
// s8 (connections and export), s9 (settings), s10 (benchmarks).

import { createServer } from 'node:net'
import { spawn } from 'node:child_process'
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { App, Evidence, children, cleanDir, http, killPid, ps, sleep, waitFor, alive } from './lib.mjs'

const here = fileURLToPath(new URL('.', import.meta.url))
const argv = process.argv.slice(2)
const scenario = argv[0]
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : dflt
}
const fresh = argv.includes('--fresh')
const exe = opt('exe', 'D:\\verne-build\\ogun\\cargo-target\\debug\\ogun-studio.exe')
const root = opt('root', 'D:\\verne-build\\ogun\\acc\\run')
const out = opt('out', 'D:\\verne-build\\ogun\\evidence')
const FIXTURE_PORT = 38417
const workflow = (name) => JSON.parse(readFileSync(join(here, 'workflows', name), 'utf8'))
const key = () => 'acc-' + Math.random().toString(36).slice(2, 12)

mkdirSync(out, { recursive: true })

async function startFixture(ev) {
  // A python fixture (dual stack). Reused when one is already up.
  try {
    const r = await http(`http://127.0.0.1:${FIXTURE_PORT}/stats`)
    if (r.status === 200) return null
  } catch {
    /* start one */
  }
  const p = spawn('python', [join(here, 'fixture_server.py'), '--port', String(FIXTURE_PORT)], {
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  })
  await waitFor(async () => {
    try {
      return (await http(`http://127.0.0.1:${FIXTURE_PORT}/stats`)).status === 200
    } catch {
      return false
    }
  }, { what: 'the fixture server' })
  ev?.note(`fixture server started (pid ${p.pid})`)
  return p
}

async function launch(ev, { keepData = true } = {}) {
  if (fresh && !keepData) cleanDir(root)
  const app = new App({ root, exe })
  await app.start()
  const info = await app.waitEngine()
  ev.note(`app pid ${app.pid}, engine ${JSON.stringify(info).slice(0, 200)}`)
  return { app, info }
}

const scenarios = {
  // ---------------------------------------------------------------------------------------------------------
  async s1(ev) {
    cleanDir(root)
    const { app, info } = await launch(ev, { keepData: false })
    ev.save('engine_info.json', info)
    ev.check('the app owns a running engine', info.state === 'running' && info.pid > 0, JSON.stringify(info))
    const engines = app.engines()
    ev.check('exactly one ogund child of the app', engines.length === 1, JSON.stringify(engines.map((e) => e.pid)))
    const eng = engines[0]
    ev.check('the engine takes no secret on its command line', eng && !/token|secret|credential/i.test(eng.cmd), eng?.cmd)
    const listening = ps(
      `Get-NetTCPConnection -State Listen -OwningProcess ${eng.pid} -ErrorAction SilentlyContinue | Measure-Object | Select-Object -ExpandProperty Count`,
    )
    ev.check('the engine listens on no TCP port (a pipe only)', listening === '0', `listening sockets: ${listening}`)
    const studio = await app.invoke('studio_info')
    ev.save('studio_info.json', studio)
    ev.check('the link is the local engine with the studio kind', studio.link === 'local' && studio.connected, JSON.stringify(studio).slice(0, 200))
    const wfs = await app.invoke('workflows_list')
    ev.check('the workspace starts empty', (wfs.workflows ?? []).length === 0, JSON.stringify(wfs).slice(0, 200))
    await app.shot(join(ev.dir, '01-empty-workspace.png'))
    const samples = await app.invoke('samples_list')
    ev.save('samples.json', samples)
    ev.check('the bundled samples are offered', samples.length >= 2, samples.map((s) => s.id).join(','))
    const imported = await app.invoke('sample_import', { id: 'bikeshare' })
    ev.save('sample_import.json', imported)
    const hashes = imported.workflows.map((w) => w.workflow_hash)
    ev.check('the sample import stores workflows with BLAKE3 hashes', imported.workflows.length >= 4 && hashes.every((h) => /^[0-9a-f]{64}$/.test(h)), `${imported.workflows.length} workflows`)
    const again = await app.invoke('sample_import', { id: 'bikeshare' })
    ev.check('importing again creates nothing new', again.workflows.every((w) => w.created === false), JSON.stringify(again.workflows.map((w) => w.created)))
    const listed = await app.invoke('workflows_list')
    ev.check('workflows.list shows them', (listed.workflows ?? []).length === imported.workflows.length)
    await app.goto('#/workflows')
    await app.shot(join(ev.dir, '02-after-sample-import.png'))
    // Quit the app the way a person does and make sure the engine goes with it; start again and find the data.
    const shellPid = app.pid
    await app.stop()
    await sleep(2500)
    ev.check('closing the app stops the engine', !alive(eng.pid), `engine pid ${eng.pid}`)
    ev.check('and the app itself', !alive(shellPid))
    const app2 = new App({ root, exe })
    await app2.start()
    const info2 = await app2.waitEngine()
    ev.check('a second launch starts a new engine', info2.pid !== info.pid)
    const wfs2 = await app2.invoke('workflows_list')
    ev.check('the workspace survived the restart', (wfs2.workflows ?? []).length === imported.workflows.length)
    await app2.shot(join(ev.dir, '03-second-launch.png'))
    await app2.stop()
    return ev.finish()
  },
}

if (!scenarios[scenario]) {
  console.error('scenarios:', Object.keys(scenarios).join(' '))
  process.exit(2)
}
const ev = new Evidence(out, scenario)
let fixture = null
try {
  fixture = await startFixture(ev)
  const ok = await scenarios[scenario](ev)
  process.exitCode = ok ? 0 : 1
} catch (e) {
  ev.note('ERROR ' + (e.stack ?? e))
  ev.check('the scenario ran to the end', false, String(e))
  ev.finish()
  process.exitCode = 1
} finally {
  if (fixture) killPid(fixture.pid)
}
