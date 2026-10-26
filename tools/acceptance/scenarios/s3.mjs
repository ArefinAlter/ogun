// Scenario 3: schedules fire while the app is open; a stream from a local producer (a file that grows) is cut into
// event-time windows, sealed, run, revised by late records, and survives a restart of the ingest bridge.
import { appendFileSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

export async function run({ ev, root, cleanDir, ps, sleep, waitFor, children, killPid, workflow, key, launch }) {
  cleanDir(root)
  const { app } = await launch(root)
  const put = (doc) => app.invoke('workflows_put', { params: { workflow: doc, idempotency_key: key() } })
  const runs = async (wf) => ((await app.invoke('runs_list', { params: { workflow: wf, limit: 100 } })).runs ?? [])
  const text = async (blake3) => {
    const g = await app.invoke('artifacts_get', { params: { blake3, inline_max_bytes: 1000000 } })
    return Buffer.from(g.content_base64 ?? '', 'base64').toString('utf8')
  }

  // ---- Schedules: a cron workflow fires by itself while the window is open.
  const cron = await put(workflow('acc-cron.workflow.json'))
  ev.save('cron-put.json', cron)
  const next = await app.engine('schedules.list', {}).catch((e) => ({ error: String(e) }))
  ev.save('schedules-list.json', next)
  ev.note('waiting for the scheduler to start two runs of acc-cron (one per minute)')
  const fired = await waitFor(async () => {
    const r = await runs('acc-cron')
    return r.length >= 2 ? r : null
  }, { ms: 200000, every: 3000, what: 'two scheduled runs' })
  ev.save('cron-runs.json', fired)
  ev.check('the scheduler started runs on its own while the app was open', fired.length >= 2, `${fired.length} runs`)
  const finishedRuns = await waitFor(async () => {
    const r = await runs('acc-cron')
    return r.every((x) => ['succeeded', 'failed', 'cancelled'].includes(x.state)) ? r : null
  }, { ms: 90000, what: 'the scheduled runs to finish' })
  ev.check('and they succeeded', finishedRuns.every((x) => x.state === 'succeeded'), finishedRuns.map((x) => x.state).join(','))
  const gap = Math.abs(new Date(fired[0].created_at) - new Date(fired[1].created_at))
  ev.check('about a minute apart', gap > 30000 && gap < 90000, `${Math.round(gap / 1000)} s`)
  const daemonLog = ps(`Get-Content '${join(root, 'home', 'profiles', 'studio', 'logs', 'ogund.log')}' -Tail 400`)
  ev.check('the daemon log attributes them to the scheduler, not to a person', /scheduler/i.test(daemonLog), '')
  // Pause it: set_enabled false on the workflow stops new runs.
  await app.engine('workflows.set_enabled', { name: 'acc-cron', enabled: false, idempotency_key: key() }).catch((e) => ev.note('set_enabled: ' + e))
  const countAtPause = (await runs('acc-cron')).length
  ev.note('paused acc-cron; waiting 70 s to see no new run')
  await sleep(70000)
  const countAfter = (await runs('acc-cron')).length
  ev.check('a paused workflow gets no new scheduled run', countAfter === countAtPause, `${countAtPause} -> ${countAfter}`)

  // ---- Streams: a local producer appends JSON Lines; the ingest bridge tails the file.
  const dir = join(root, 'streams')
  mkdirSync(dir, { recursive: true })
  const file = join(dir, 'events.jsonl')
  rmSync(file, { force: true })
  writeFileSync(file, '')
  const wf = workflow('acc-stream.workflow.json')
  wf.nodes[0].config.path = file.replace(/\\/g, '/')
  const sput = await put(wf)
  ev.save('stream-put.json', sput)
  const envs = await app.engine('environments.list', {})
  ev.save('environments.json', envs)
  const ingest = (envs.bridges ?? []).find((b) => /ingest/i.test(b.name ?? b.id ?? ''))
  ev.check('the ingest bridge is supervised by the app and reports running', ingest && ingest.state === 'running', JSON.stringify(ingest))

  const T0 = Math.floor((Date.now() - 30 * 60000) / 60000) * 60000
  let seq = 0
  const line = (station, ts, bikes, s = ++seq) => JSON.stringify({ station_id: station, seq: s, ts_ms: ts, bikes }) + '\n'
  const batch = []
  for (let w = 0; w < 3; w++) {
    for (let i = 0; i < 5; i++) {
      batch.push(line('S1', T0 + w * 60000 + i * 1000, 5 + i))
      batch.push(line('S2', T0 + w * 60000 + i * 1000 + 500, 10 + i))
    }
  }
  appendFileSync(file, batch.join(''))
  // A partial last line is not read until it is complete.
  appendFileSync(file, '{"station_id":"S1","seq":9001,"ts_ms":' + (T0 + 5000))
  // An event far ahead moves the watermark past the three windows.
  appendFileSync(file, '\n' + line('S1', T0 + 10 * 60000, 7, 5000))
  ev.note('producer appended 30 records across 3 windows, a partial line and one far-ahead record')
  const sealed = await waitFor(async () => {
    const r = await runs('acc-stream')
    return r.length >= 3 ? r : null
  }, { ms: 120000, every: 2000, what: 'three window runs' })
  ev.save('stream-runs.json', sealed)
  ev.check('three event-time windows sealed and started their runs', sealed.length >= 3, `${sealed.length} runs`)
  const done = await waitFor(async () => {
    const r = await runs('acc-stream')
    return r.length >= 3 && r.every((x) => ['succeeded', 'failed', 'cancelled'].includes(x.state)) ? r : null
  }, { ms: 120000, every: 2000, what: 'the window runs to finish' })
  ev.check('and they succeeded', done.every((x) => x.state === 'succeeded'), done.map((x) => x.state).join(','))
  const st = await app.invoke('streams_list')
  ev.save('streams-list.json', st)
  const status = await app.invoke('streams_status', { params: { workflow: 'acc-stream', node: 'events' } })
  ev.save('streams-status.json', status)
  // The rollup of the first window.
  const first = done[done.length - 1]
  const arts = (await app.invoke('artifacts_list', { params: { run_id: first.run_id } })).artifacts
  const rollupArt = arts.find((a) => a.node === 'rollup')
  const rollupText = rollupArt ? await text(rollupArt.blake3) : ''
  ev.save('rollup-first-window.jsonl', rollupText)
  const s1 = rollupText.split('\n').filter(Boolean).map((l) => JSON.parse(l)).find((r) => r.station_id === 'S1')
  ev.check('the first window rolled up its five records for station S1 and no more', s1 && JSON.stringify(s1).includes('"count":5'), JSON.stringify(s1))
  const hitsAfter = (await app.invoke('partitions_status', { params: { workflow: 'acc-stream' } }))
  ev.save('stream-partitions.json', hitsAfter)

  // Late records: an event for the first window arrives after it was sealed (allowed lateness is an hour).
  const before = (await runs('acc-stream')).length
  appendFileSync(file, line('S1', T0 + 30000, 99))
  // A duplicate of a record already ingested (same station and seq) must not count twice.
  appendFileSync(file, line('S2', T0 + 500, 10, 1))
  ev.note('producer appended one late record for the first window and one duplicate')
  const revised = await waitFor(async () => {
    const r = await runs('acc-stream')
    return r.length > before ? r : null
  }, { ms: 90000, every: 2000, what: 'a revision run for the late record' }).catch(() => null)
  ev.check('a late record revised its window: a new run followed', !!revised, `${before} -> ${revised?.length}`)
  if (revised) {
    await waitFor(async () => {
      const r = await runs('acc-stream')
      return r.every((x) => ['succeeded', 'failed', 'cancelled'].includes(x.state)) ? r : null
    }, { ms: 90000, what: 'the revision run to finish' })
    const newest = (await runs('acc-stream'))[0]
    const a2 = (await app.invoke('artifacts_list', { params: { run_id: newest.run_id } })).artifacts
    const r2 = a2.find((a) => a.node === 'rollup')
    const t2 = r2 ? await text(r2.blake3) : ''
    ev.save('rollup-revision.jsonl', t2)
    const s1b = t2.split('\n').filter(Boolean).map((l) => JSON.parse(l)).find((r) => r.station_id === 'S1')
    ev.check('the revision counts six records for S1 (five and the late one), and the duplicate was not counted', s1b && JSON.stringify(s1b).includes('"count":6'), JSON.stringify(s1b))
  }

  // The ingest bridge dies; the app restarts it and the stream carries on without repeating records.
  const kids = children(app.pid).filter((c) => /ogun-ingest/i.test(c.name))
  ev.check('the ingest bridge is a child of the app', kids.length === 1, JSON.stringify(kids.map((k) => k.pid)))
  killPid(kids[0].pid)
  ev.note('killed ogun-ingest; waiting for the app to bring it back')
  const kids2 = await waitFor(async () => {
    const k = children(app.pid).filter((c) => /ogun-ingest/i.test(c.name))
    return k.length === 1 && k[0].pid !== kids[0].pid ? k : null
  }, { ms: 60000, every: 1000, what: 'the bridge to be restarted' })
  ev.check('the app restarted the ingest bridge', kids2.length === 1, `${kids[0].pid} -> ${kids2[0].pid}`)
  const countBefore = (await runs('acc-stream')).length
  appendFileSync(file, line('S2', T0 + 2 * 60000 + 59000, 3))
  await sleep(15000)
  appendFileSync(file, line('S1', T0 + 11 * 60000, 8))
  const afterRestart = await waitFor(async () => {
    const r = await runs('acc-stream')
    return r.length > countBefore ? r : null
  }, { ms: 90000, every: 2000, what: 'a run after the bridge restart' }).catch(() => null)
  ev.check('records appended after the restart were ingested (a window ran again)', !!afterRestart, `${countBefore} -> ${afterRestart?.length}`)
  await app.shot(join(ev.dir, '09-streams.png'))
  await app.stop()
  return ev.finish()
}
