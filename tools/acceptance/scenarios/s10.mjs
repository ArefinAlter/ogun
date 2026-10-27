// Scenario 10: the benchmark profile from the app. What runs is the sample suite (four tasks in the Spider 2.0-DBT
// layout, a scripted reference agent and an idle one) on the local runtime inside a Windows AppContainer with no
// network; the Harbor, Spider 2.0-DBT and ADE-bench adapters are in the engine but not offered, and no model is
// involved. Checked: results, isolation, the loopback probe, hash-chained trajectories, and that the run touched
// nothing of the app's own stores.
import { createServer } from 'node:net'
import { spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

export async function run({ ev, exe, root, cleanDir, launch, sleep, waitFor, key }) {
  cleanDir(root)
  const { app } = await launch(root)
  const list = await app.engine('bench.list', {})
  ev.save('bench-list.json', list)
  const local = list.runtimes.find((r) => r.id === 'local')
  ev.check('the engine reports the local runtime with its isolation kind', local?.available === true && local.isolation === 'local_appcontainer', JSON.stringify(local))
  ev.check('and says the sample suite is scripted agents, not models', /scripted agents, not models/.test(JSON.stringify(list.suites)), '')
  const runsBefore = ((await app.invoke('runs_list', { params: {} })).runs ?? []).length
  const wfBefore = ((await app.invoke('workflows_list')).workflows ?? []).length

  await app.goto('#/benchmarks')
  await sleep(1500)
  await app.shot(join(ev.dir, '10-benchmarks-before.png'))

  const started = await app.engine('bench.run', { suite: 'sample', runtime: 'local', seeds: [1], idempotency_key: key() })
  ev.save('bench-run.json', started)
  const done = await waitFor(async () => {
    const s = await app.engine('bench.status', { id: started.id })
    return s.run.state !== 'running' ? s : null
  }, { ms: 240000, every: 2000, what: 'the benchmark run' })
  ev.save('bench-status.json', done)
  ev.check('the run finished', done.run.state === 'finished', `${done.run.state} ${done.run.error ?? ''}`)
  ev.check('eight runs: four tasks for each of two agents', done.rows.length === 8, `${done.rows.length}`)
  const ref = done.rows.filter((r) => r.agent === 'reference-script')
  const idle = done.rows.filter((r) => r.agent === 'idle-script')
  ev.check('the reference script passed every task, including the network task', ref.length === 4 && ref.every((r) => r.passed), ref.map((r) => `${r.task_id}:${r.passed}`).join(' '))
  ev.check('the idle script passed none', idle.length === 4 && idle.every((r) => !r.passed), idle.map((r) => `${r.task_id}:${r.passed}`).join(' '))
  ev.check('every run says how it was isolated: an AppContainer with the network denied', done.rows.every((r) => r.isolation === 'local_appcontainer'), [...new Set(done.rows.map((r) => r.isolation))].join(','))
  ev.check('the engine opened a loopback listener for the network task and nothing connected to it', done.run.probe_hits === 0, `${done.run.probe_hits} connections`)

  // The control: the same script outside the sandbox does reach the listener, so the probe means something.
  let hits = 0
  const l = createServer((s) => {
    hits++
    s.destroy()
  })
  await new Promise((r) => l.listen(0, '127.0.0.1', r))
  const ws = join(root, 'control')
  mkdirSync(ws, { recursive: true })
  const bench = join(exe, '..', 'ogun-bench.exe')
  const c = spawnSync(bench, ['sample-agent', 'solve', '--task', 'network-denied', '--workspace', ws, '--trajectory', join(ws, 'traj.jsonl')], {
    env: { ...process.env, PROBE_ADDR: `127.0.0.1:${l.address().port}` },
    encoding: 'utf8',
  })
  const answer = existsSync(join(ws, 'answer.txt')) ? readFileSync(join(ws, 'answer.txt'), 'utf8').trim() : ''
  ev.check('control: outside the sandbox the same script reaches the listener and answers "reachable"', c.status === 0 && answer === 'reachable' && hits >= 1, `answer ${answer}, hits ${hits}`)
  l.close()

  // The trajectories are hash-chained and verify.
  const row = ref[0]
  const replay = await app.engine('bench.replay', { id: started.id, agent_run: row.agent_run })
  ev.save('bench-replay.json', { verified: replay.verified, summary: replay.summary, events: replay.events?.length })
  ev.check('a trajectory verifies as a hash chain', replay.verified === true && (replay.events ?? []).length >= 5, `${replay.events?.length} events`)

  // Isolation from the app's own state.
  const runsAfter = ((await app.invoke('runs_list', { params: {} })).runs ?? []).length
  const wfAfter = ((await app.invoke('workflows_list')).workflows ?? []).length
  ev.check('the benchmark added no run and no workflow to the app', runsAfter === runsBefore && wfAfter === wfBefore, `runs ${runsBefore}->${runsAfter}, workflows ${wfBefore}->${wfAfter}`)
  const benchRoot = join(root, 'bench')
  ev.check('its files are outside the engine state directory', existsSync(benchRoot) && !benchRoot.startsWith(join(root, 'home')), benchRoot)
  const refuse = spawnSync(bench, ['--state-dir', join(root, 'home', 'profiles', 'studio', 'bench'), 'sample-matrix', '--dir', join(root, 'x')], { env: { ...process.env, OGUN_HOME: join(root, 'home') }, encoding: 'utf8' })
  void refuse
  const runWithin = spawnSync(bench, ['--state-dir', join(root, 'home', 'profiles', 'studio', 'inside'), 'run', '--matrix', join(benchRoot, 'runs', started.id, 'matrix.json'), '--out', join(root, 'x-out')], { env: { ...process.env, OGUN_HOME: join(root, 'home') }, encoding: 'utf8' })
  ev.save('bench-inside-state-refused.txt', `exit ${runWithin.status}\n${runWithin.stderr.slice(0, 600)}`)
  ev.check('and a benchmark pointed at a state directory inside the engine state is refused', runWithin.status !== 0 && /overlap|isolat/i.test(runWithin.stderr), runWithin.stderr.slice(0, 200))

  await app.goto('#/benchmarks')
  await sleep(1500)
  await app.shot(join(ev.dir, '10-benchmarks-after.png'))
  await app.stop()
  return ev.finish()
}
