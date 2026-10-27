// Scenario 2, second part: the core operators on two real public JSON APIs (GitHub's releases endpoint and the
// USGS earthquake catalog), separate from the local-fixture part so that a rate limit of a public service cannot hide the
// rest. Each run is kept as evidence; a failure of the public service is recorded, not retried silently.
export async function run({ ev, root, cleanDir, launch, waitFor, key }) {
  cleanDir(root)
  const { app } = await launch(root)
  await app.invoke('sample_import', { id: 'starter' })
  const finish = (id, ms = 180000) =>
    waitFor(async () => {
      const r = await app.invoke('runs_get', { params: { run_id: id } })
      return ['succeeded', 'failed', 'cancelled'].includes(r.state) ? r : null
    }, { ms, every: 1000, what: 'a run' })
  const text = async (blake3) => {
    const g = await app.invoke('artifacts_get', { params: { blake3, inline_max_bytes: 4000000 } })
    return Buffer.from(g.content_base64 ?? '', 'base64').toString('utf8')
  }
  const errors = (r) => JSON.stringify((r.nodes ?? []).filter((n) => n.error).map((n) => n.error))

  // GitHub: the releases of this repository, through acquire, parse, filter, partition, hash and manifest.
  const gh = await finish((await app.invoke('runs_start', { params: { workflow: 'github-releases', partitions: { kind: 'latest' }, idempotency_key: key() } })).run_id)
  ev.save('github-run.json', gh)
  ev.check('the GitHub releases workflow runs against the real API', gh.state === 'succeeded', gh.state + ' ' + errors(gh))
  if (gh.state === 'succeeded') {
    const arts = (await app.invoke('artifacts_list', { params: { run_id: gh.run_id } })).artifacts
    const fetched = await text(arts.find((a) => a.node === 'fetch').blake3)
    ev.save('github-releases-sample.json', fetched.slice(0, 3000))
    ev.check('the fetched document is GitHub JSON that names the v0.0.1a release', /v0\.0\.1a/.test(fetched), `${fetched.length} bytes`)
    const rows = (await text(arts.find((a) => a.node === 'rows').blake3)).split('\n').filter(Boolean)
    ev.check('parse made one row per release', rows.length >= 1 && rows.every((l) => JSON.parse(l).tag_name), `${rows.length} rows`)
  }

  // USGS: a daily partitioned workflow, three days (Open-Meteo is not used: its robots.txt disallows everything, and acquire honours robots.txt).
  const wx = await finish((await app.invoke('runs_start', { params: { workflow: 'earthquakes-daily', partitions: { kind: 'keys', keys: ['2026-01-01', '2026-01-02', '2026-01-03'] }, idempotency_key: key() } })).run_id, 240000)
  ev.save('earthquakes-run.json', wx)
  ev.check('the earthquakes workflow runs three daily partitions against the real USGS API', wx.state === 'succeeded', wx.state + ' ' + errors(wx))
  if (wx.state === 'succeeded') {
    const arts = (await app.invoke('artifacts_list', { params: { run_id: wx.run_id } })).artifacts
    const fetched = arts.filter((a) => a.node === 'fetch')
    const body = await text(fetched[0].blake3)
    ev.save('earthquakes-sample.json', body.slice(0, 1500))
    ev.check('three documents came back, and they are USGS GeoJSON', fetched.length === 3 && /"type":"FeatureCollection"/.test(body) && /usgs/i.test(body), `${fetched.length} fetched`)
    const st = await app.invoke('partitions_status', { params: { workflow: 'earthquakes-daily', from: '2026-01-01', to: '2026-01-03' } })
    ev.check('the three partitions are done', (st.ranges ?? []).filter((r) => r.state === 'done').reduce((n, r) => n + r.count, 0) === 3, JSON.stringify(st.ranges))
  }
  await app.stop()
  return ev.finish()
}
