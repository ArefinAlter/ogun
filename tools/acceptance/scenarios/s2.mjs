// Scenario 2: the core operators end to end on real inputs (a local fixture and the real GitHub API), artifacts
// and lineage, a 121-partition backfill that survives killing ogund and, separately, the whole app.
import { rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

export async function run({ port, ev, exe, root, cleanDir, App, ps, alive, sleep, waitFor, http, killPid, workflow, key, launch, FIXTURE_PORT }) {
  cleanDir(root)
  const ogun = join(exe, '..', 'ogun.exe')
  const { app } = await launch(root)
  await app.engine('runtime.set', { patch: { concurrency: 8 }, idempotency_key: key() })
  const stats = async () => (await http(`http://127.0.0.1:${FIXTURE_PORT}/stats`)).json ?? {}
  const dayCount = (st) => Object.keys(st).filter((k) => k.startsWith('/daily/')).length
  const put = (a, name) => a.invoke('workflows_put', { params: { workflow: workflow(name), idempotency_key: key() } })
  const start = (a, wf, partitions = { kind: 'latest' }) =>
    a.invoke('runs_start', { params: { workflow: wf, partitions, idempotency_key: key() } })
  const tryStart = (a, wf, partitions) => start(a, wf, partitions).then((v) => ({ ok: v }), (e) => ({ err: e.envelope ?? String(e) }))
  const doneIn = async (a, from, to) => {
    const st = await a.invoke('partitions_status', { params: { workflow: 'acc-daily', from, to } })
    return (st.ranges ?? []).filter((r) => r.state === 'done').reduce((n, r) => n + r.count, 0)
  }
  const finished = (r) => ['succeeded', 'failed', 'cancelled'].includes(r.state)
  const waitRun = (a, id, ms = 120000) =>
    waitFor(async () => {
      const r = await a.invoke('runs_get', { params: { run_id: id } })
      return finished(r) ? r : null
    }, { ms, what: `run ${id}` })
  const hashFile = (path) => ps(`& '${ogun}' hash-file '${path}'`).split(/\s+/)[0]
  const text = async (a, blake3, max = 4000000) => {
    const g = await a.invoke('artifacts_get', { params: { blake3, inline_max_bytes: max } })
    return Buffer.from(g.content_base64 ?? '', 'base64').toString('utf8')
  }

  // 1. A local fixture through every core operator.
  const putResult = await put(app, 'acc-local-fixture.workflow.json')
  ev.save('put-local-fixture.json', putResult)
  const run1 = await start(app, 'acc-local-fixture')
  const done1 = await waitRun(app, run1.run_id)
  ev.save('run-local-fixture.json', done1)
  ev.check('acquire, parse, filter, partition, hash and manifest run to success on a local fixture', done1.state === 'succeeded', done1.state + ' ' + JSON.stringify((done1.nodes ?? []).filter((n) => n.error).map((n) => n.error)))
  const arts1 = await app.invoke('artifacts_list', { params: { run_id: run1.run_id } })
  ev.save('artifacts-local-fixture.json', arts1)
  const list1 = arts1.artifacts ?? arts1
  ev.check('the run produced artifacts', list1.length >= 5, `${list1.length} artifacts`)
  let verified = 0
  const tmp = join(ev.dir, 'artifact-bytes.tmp')
  for (const a of list1.slice(0, 12)) {
    const got = await app.invoke('artifacts_get', { params: { blake3: a.blake3, inline_max_bytes: 4000000 } })
    writeFileSync(tmp, Buffer.from(got.content_base64 ?? '', 'base64'))
    if (hashFile(tmp) === a.blake3) verified++
  }
  rmSync(tmp, { force: true })
  ev.check('the bytes of every artifact hash to its BLAKE3 digest (checked with the ogun CLI)', verified === Math.min(12, list1.length), `${verified} checked`)
  const rowsArt = list1.find((a) => a.node === 'rows')
  const rowsText = await text(app, rowsArt.blake3)
  const rows = rowsText.split('\n').filter(Boolean).map((l) => JSON.parse(l))
  ev.save('rows.jsonl', rowsText)
  ev.check('parse turned the JSON array into 10 rows', rows.length === 10 && rows[0].tag_name === 'v0.0.1', `${rows.length} rows`)
  const pubArt = list1.find((a) => a.node === 'published')
  const pub = (await text(app, pubArt.blake3)).split('\n').filter(Boolean)
  ev.check('filter dropped the one draft', pub.length === 9, `${pub.length} rows`)
  const lineage = await app.invoke('lineage_get', { params: { run_id: run1.run_id } })
  ev.save('lineage-local-fixture.json', lineage)
  ev.check('lineage events (OpenLineage) were recorded for the run', (lineage.events ?? []).length >= 1, `${(lineage.events ?? []).length} events`)

  // 2. The real public API: the starter workflow against GitHub's releases endpoint.
  await app.invoke('sample_import', { id: 'starter' })
  let run2 = await start(app, 'github-releases')
  let done2 = await waitRun(app, run2.run_id, 180000)
  if (done2.state !== 'succeeded') {
    // A public API can fail for a moment (rate limit, network): keep what happened, wait, and try once more.
    ev.save('run-github-first-attempt.json', done2)
    const evs = await app.invoke('events_since', { params: { run_id: run2.run_id, after: 0, limit: 200, wait_ms: 250 } })
    ev.save('run-github-first-attempt-events.json', evs)
    await sleep(20000)
    run2 = await start(app, 'github-releases')
    done2 = await waitRun(app, run2.run_id, 180000)
  }
  ev.save('run-github.json', done2)
  ev.check('the GitHub releases workflow runs against the real API', done2.state === 'succeeded', done2.state + ' ' + JSON.stringify((done2.nodes ?? []).filter((n) => n.error).map((n) => n.error)))
  if (done2.state === 'succeeded') {
    const arts2 = (await app.invoke('artifacts_list', { params: { run_id: run2.run_id } })).artifacts
    const fetched = arts2.find((a) => a.node === 'fetch')
    const body = await text(app, fetched.blake3)
    ev.save('github-releases-sample.json', body.slice(0, 4000))
    ev.check('the fetched document is GitHub JSON that names the v0.0.1a release', /v0\.0\.1a/.test(body), `${body.length} bytes`)
  }

  // 3. A 121-partition backfill; kill ogund in the middle.
  await put(app, 'acc-daily.workflow.json')
  const plan = await app.invoke('backfills_plan', { params: { workflow: 'acc-daily', from: '2026-01-01', to: '2026-05-01' } })
  ev.save('backfill-plan.json', plan)
  ev.check('the plan covers 121 partitions', plan.to_run === 121, JSON.stringify(plan).slice(0, 200))
  await http(`http://127.0.0.1:${FIXTURE_PORT}/admin/reset`, { method: 'POST' })
  const bf = await start(app, 'acc-daily', { kind: 'range', from: '2026-01-01', to: '2026-05-01' })
  ev.save('backfill-start.json', bf)
  await waitFor(async () => dayCount(await stats()) >= 40, { ms: 400000, every: 2000, what: '40 partitions to start' })
  const engine = app.engines()[0]
  const before = await app.invoke('engine_info')
  killPid(engine.pid)
  ev.note(`killed ogund pid ${engine.pid}`)
  const after = await waitFor(async () => {
    try {
      const i = await app.invoke('engine_info')
      return i.state === 'running' && i.pid !== before.pid ? i : null
    } catch {
      return null
    }
  }, { ms: 60000, what: 'the supervised engine restart' })
  ev.save('engine-after-kill.json', after)
  ev.check('the app restarted the engine by itself', after.pid !== before.pid, `${before.pid} -> ${after.pid}, restarts ${after.restarts ?? '?'}`)
  const st1 = await app.invoke('partitions_status', { params: { workflow: 'acc-daily' } })
  ev.save('partitions-after-kill.json', st1)
  const doneRanges = (st1.ranges ?? []).filter((r) => r.state === 'done')
  const doneCount = doneRanges.reduce((n, r) => n + r.count, 0)
  ev.check('some partitions finished before the kill and some did not', doneCount > 0 && doneCount < 121, `${doneCount} done`)
  ev.save('fixture-hits-at-kill.json', await stats())
  // The engine resumes the runs that were in flight (resume_on_start). Asking for the same range again must not
  // start the work a second time: the partitions another run is working on are left to it.
  const again = await tryStart(app, 'acc-daily', { kind: 'range', from: '2026-01-01', to: '2026-05-01' })
  ev.save('backfill-asked-again.json', again)
  ev.check('asking for the range again right after the restart starts no duplicate work', again.err ? /already being worked on|up to date/.test(JSON.stringify(again.err)) : again.ok.partition_count < 121 - doneCount + 1, JSON.stringify(again).slice(0, 240))
  await waitFor(async () => (await doneIn(app, '2026-01-01', '2026-05-01')) === 121, { ms: 900000, every: 3000, what: 'all 121 partitions to finish' })
  const st2 = await app.invoke('partitions_status', { params: { workflow: 'acc-daily', from: '2026-01-01', to: '2026-05-01' } })
  ev.save('partitions-final.json', st2)
  ev.check('all 121 partitions are done', (await doneIn(app, '2026-01-01', '2026-05-01')) === 121)
  const hits = await stats()
  ev.save('fixture-hits-final.json', hits)
  const days = Object.entries(hits).filter(([k]) => k.startsWith('/daily/'))
  const refetched = days.filter(([, n]) => n > 1).length
  ev.check('every partition was fetched, and only the ones in flight at the kill more than once', days.length === 121 && refetched <= 8, `${days.length} days, ${refetched} fetched twice`)
  const finishedBefore = doneRanges.flatMap((r) => {
    const out = []
    const d = new Date(r.from + 'T00:00:00Z')
    const e = new Date(r.to + 'T00:00:00Z')
    for (; d <= e; d.setUTCDate(d.getUTCDate() + 1)) out.push(d.toISOString().slice(0, 10))
    return out
  })
  const reran = finishedBefore.filter((day) => (hits[`/daily/${day}.json`] ?? 0) > 1)
  ev.check('no partition that was done before the kill ran again', reran.length === 0, `${finishedBefore.length} done before; reran: ${reran.join(',')}`)
  const third = await tryStart(app, 'acc-daily', { kind: 'range', from: '2026-01-01', to: '2026-05-01' })
  ev.save('backfill-third.json', third)
  const hits3 = await stats()
  ev.check('a further backfill has nothing to do', !!third.err && /up to date/.test(JSON.stringify(third.err)) && JSON.stringify(hits3) === JSON.stringify(hits), JSON.stringify(third).slice(0, 200))

  // 4. The whole app killed in the middle of another backfill.
  await http(`http://127.0.0.1:${FIXTURE_PORT}/admin/reset`, { method: 'POST' })
  const range2 = { kind: 'range', from: '2026-05-02', to: '2026-08-30' }
  const bf4 = await start(app, 'acc-daily', range2)
  ev.save('backfill2-start.json', bf4)
  await waitFor(async () => dayCount(await stats()) >= 30, { ms: 400000, every: 2000, what: '30 partitions of the second backfill' })
  const shellPid = app.pid
  const engineBeforeAppKill = app.engines()[0]?.pid
  killPid(shellPid)
  ev.note(`killed the whole app (pid ${shellPid}) and its engine`)
  await sleep(2500)
  ev.check('the app and its engine are gone', !alive(shellPid) && !(engineBeforeAppKill && alive(engineBeforeAppKill)))
  const app2 = new App({ root, exe, port })
  await app2.start()
  const info2 = await app2.waitEngine()
  ev.note(`relaunched; engine ${info2.pid}`)
  ev.save('partitions-after-app-kill.json', await app2.invoke('partitions_status', { params: { workflow: 'acc-daily' } }))
  const hitsKill2 = await stats()
  const askedAfterKill = await tryStart(app2, 'acc-daily', range2)
  ev.save('backfill2-asked-again.json', askedAfterKill)
  await waitFor(async () => (await doneIn(app2, range2.from, range2.to)) === 121, { ms: 900000, every: 3000, what: 'all 121 days of the second range to finish' })
  ev.check('after the whole app was killed, the backfill finishes on the next launch', true, JSON.stringify(askedAfterKill).slice(0, 160))
  const hits5 = await stats()
  const days5 = Object.entries(hits5).filter(([k]) => k.startsWith('/daily/'))
  const twice = days5.filter(([, n]) => n > 1).length
  ev.save('fixture-hits-app-kill.json', { atKill: hitsKill2, final: hits5 })
  ev.check('all 121 days of the second range were fetched, only those in flight at the kill more than once', days5.length === 121 && twice <= 8, `${days5.length} days, ${twice} twice`)
  await app2.shot(join(ev.dir, '10-after-app-kill.png'))
  await app2.stop()
  return ev.finish()
}
