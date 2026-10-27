// The standalone acceptance run for Ogun Studio 0.0.1b. Each scenario (scenarios/<name>.mjs) starts the real
// desktop app on a scratch profile, drives it, and writes evidence under <out>/<scenario>/.
//
//   node acceptance.mjs <scenario> [--exe path] [--root path] [--out path]
//
// s1 install, launch, empty workspace, sample      s2 core operators, backfill, kills
// s3 schedules and streams                         s4 approvals
// s5 code operators and the sandbox                s6 agent threads
// s7 hub sign-in                                   s8 connections and export
// s9 settings                                      s10 benchmarks

import { spawn } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { App, Evidence, alive, children, cleanDir, http, killPid, ps, sleep, started, waitFor } from './lib.mjs'

const here = fileURLToPath(new URL('.', import.meta.url))
const argv = process.argv.slice(2)
const scenario = argv[0]
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`)
  return i >= 0 ? argv[i + 1] : dflt
}
const exe = opt('exe', 'D:\\verne-build\\ogun\\cargo-target\\debug\\ogun-studio.exe')
const root = opt('root', 'D:\\verne-build\\ogun\\acc\\run')
const out = opt('out', 'D:\\verne-build\\ogun\\evidence')
const FIXTURE_PORT = 38417
const port = Number(opt('port', '9223'))

mkdirSync(out, { recursive: true })

const file = join(here, 'scenarios', `${scenario}.mjs`)
if (!scenario || !existsSync(file)) {
  console.error('usage: node acceptance.mjs <s1..s10>')
  process.exit(2)
}

async function startFixture(ev) {
  try {
    if ((await http(`http://127.0.0.1:${FIXTURE_PORT}/stats`)).status === 200) return null
  } catch {
    /* start one */
  }
  const p = spawn('python', [join(here, 'fixture_server.py'), '--port', String(FIXTURE_PORT)], {
    stdio: 'ignore',
    windowsHide: true,
  })
  await waitFor(async () => {
    try {
      return (await http(`http://127.0.0.1:${FIXTURE_PORT}/stats`)).status === 200
    } catch {
      return false
    }
  }, { what: 'the fixture server' })
  ev.note(`fixture server started (pid ${p.pid})`)
  return p
}

const ev = new Evidence(out, scenario)
const ctx = {
  ev,
  exe,
  root,
  out,
  here,
  FIXTURE_PORT,
  App,
  alive,
  children,
  cleanDir,
  http,
  killPid,
  ps,
  sleep,
  waitFor,
  workflow: (name) => JSON.parse(readFileSync(join(here, 'workflows', name), 'utf8')),
  key: () => 'acc-' + Math.random().toString(36).slice(2, 12),
  port,
  async launch(dir = root) {
    const app = new App({ root: dir, exe, port })
    await app.start()
    const info = await app.waitEngine()
    ev.note(`app pid ${app.pid}, engine ${JSON.stringify(info).slice(0, 200)}`)
    return { app, info }
  },
}
let fixture = null
try {
  fixture = await startFixture(ev)
  const mod = await import(pathToFileURL(file).href)
  const ok = await mod.run(ctx)
  process.exitCode = ok ? 0 : 1
} catch (e) {
  ev.note('ERROR ' + (e.stack ?? e))
  ev.check('the scenario ran to the end', false, String(e))
  ev.finish()
  process.exitCode = 1
} finally {
  for (const pid of started) killPid(pid)
  if (fixture) killPid(fixture.pid)
  process.exit(process.exitCode ?? 0)
}
