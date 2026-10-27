// Scenario 9: settings persist across restarts (run defaults, evidence, agent limits, storage rules, appearance),
// the evidence bundle downloads from a run page, and the storage rules produce a cleanup plan that a cleanup then
// carries out for real. Runs of the cleanup are made to look 40 days old by OGUN_STORAGE_CLOCK_OFFSET_DAYS, an
// acceptance hook of the engine (the removal itself, in the artifact store on disk, is real).
import { existsSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'

function dirBytes(dir) {
  let n = 0
  if (!existsSync(dir)) return 0
  for (const e of readdirSync(dir)) {
    const p = join(dir, e)
    try {
      const st = statSync(p)
      n += st.isDirectory() ? dirBytes(p) : st.size
    } catch {
      /* locked */
    }
  }
  return n
}

export async function run({ port, ev, exe, root, cleanDir, App, launch, sleep, waitFor, http, workflow, key, FIXTURE_PORT }) {
  cleanDir(root)
  let { app } = await launch(root)
  const E = (m, p = {}) => app.engine(m, p)
  const finish = (id) => waitFor(async () => {
    const r = await app.invoke('runs_get', { params: { run_id: id } })
    return ['succeeded', 'failed', 'cancelled'].includes(r.state) ? r : null
  }, { ms: 90000, every: 800, what: 'a run' })
  await app.invoke('workflows_put', { params: { workflow: workflow('acc-local-fixture.workflow.json'), idempotency_key: key() } })
  const failing = workflow('acc-local-fixture.workflow.json')
  failing.workflow = 'acc-fail'
  failing.nodes[0].config.allowed_domains = ['example.invalid']
  await app.invoke('workflows_put', { params: { workflow: failing, idempotency_key: key() } })
  const runs = []
  for (let i = 0; i < 2; i++) runs.push(await finish((await app.invoke('runs_start', { params: { workflow: 'acc-local-fixture', partitions: { kind: 'latest' }, idempotency_key: key() } })).run_id))
  const bad = await finish((await app.invoke('runs_start', { params: { workflow: 'acc-fail', partitions: { kind: 'latest' }, idempotency_key: key() } })).run_id)
  ev.check('two runs succeeded and one failed on purpose', runs.every((r) => r.state === 'succeeded') && bad.state === 'failed', `${runs.map((r) => r.state)} / ${bad.state}`)

  // 1. Change settings in each section.
  await E('runtime.set', { patch: { concurrency: 6, retries: 1, approval_expiry_hours: 24 }, idempotency_key: key() })
  await E('evidence.set', { patch: { format: 'html', include_lineage: false }, idempotency_key: key() })
  await E('agent.settings.put', { patch: { instructions: 'Answer briefly and name the workflow you mean.', spend_cap_usd: 0.5, day_cap_usd: 2, retention_days: 30 }, idempotency_key: key() })
  await E('storage.set_rules', { patch: { retention_days: 30, delete_failed_after_days: 7, delete_partials_after_days: 7, clear_caches_after_days: 3, keep_lineage_referenced: true }, idempotency_key: key() })
  await app.invoke('shell_prefs_set', { prefs: { keep_in_tray: true } })
  // Appearance and keybindings (kept by the window itself).
  await app.goto('#/settings/appearance')
  await sleep(800)
  const darkCard = app.page.getByText('Dark', { exact: true }).first()
  let appearanceChanged = false
  try {
    await darkCard.click({ timeout: 5000 })
    await sleep(400)
    appearanceChanged = (await app.page.evaluate(() => document.documentElement.dataset.scheme)) === 'dark'
  } catch (e) {
    ev.note('appearance click: ' + e)
  }
  ev.check('the appearance setting changes the window', appearanceChanged)
  const prefsBefore = await app.page.evaluate(() => window.localStorage.getItem('ogun-studio:prefs:v1'))
  ev.save('prefs-before.json', prefsBefore ?? '')
  await app.shot(join(ev.dir, '09-appearance.png'))

  // 2. Restart and read everything back.
  await app.stop()
  await sleep(2500)
  app = new App({ root, exe, port })
  await app.start()
  await app.waitEngine()
  const E2 = (m, p = {}) => app.engine(m, p)
  const rt = await E2('runtime.get', {})
  const evd = await E2('evidence.get', {})
  const ag = await E2('agent.settings.get', {})
  const sr = await E2('storage.rules', {})
  const sp = await app.invoke('shell_prefs_get')
  ev.save('settings-after-restart.json', { rt, evd, ag, sr, sp })
  const runtime = rt.settings ?? rt.defaults ?? rt
  ev.check('run defaults persisted', JSON.stringify(rt).includes('"concurrency":6') && JSON.stringify(rt).includes('"approval_expiry_hours":24'), JSON.stringify(runtime).slice(0, 160))
  ev.check('evidence settings persisted', evd.format === 'html' && evd.include_lineage === false, JSON.stringify(evd))
  ev.check('agent settings persisted (caps and standing instructions)', ag.spend_cap_usd === 0.5 && ag.day_cap_usd === 2 && /Answer briefly/.test(ag.instructions) && ag.retention_days === 30, JSON.stringify(ag).slice(0, 200))
  ev.check('storage rules persisted', sr.retention_days === 30 && sr.delete_failed_after_days === 7 && sr.clear_caches_after_days === 3, JSON.stringify(sr).slice(0, 240))
  ev.check('shell preference persisted', sp.keep_in_tray === true)
  const prefsAfter = await app.page.evaluate(() => window.localStorage.getItem('ogun-studio:prefs:v1'))
  ev.check('the appearance setting persisted in the window', prefsAfter === prefsBefore && /dark/.test(prefsAfter ?? ''), (prefsAfter ?? '').slice(0, 160))
  await app.invoke('shell_prefs_set', { prefs: { keep_in_tray: false } })

  // 3. The evidence bundle from a run page (the file the person saves).
  const grab = async (runId) => {
    await app.page.evaluate(() => {
      window.__blobs = []
      const orig = URL.createObjectURL.bind(URL)
      URL.createObjectURL = (b) => {
        window.__blobs.push(b)
        return orig(b)
      }
    })
    await app.goto('#/overview')
    await sleep(600)
    await app.goto(`#/runs/${runId}`)
    await sleep(1500)
    await app.page.getByRole('button', { name: 'Download evidence' }).click()
    await sleep(1500)
    return app.page.evaluate(async () => (window.__blobs.length ? await window.__blobs[window.__blobs.length - 1].text() : ''))
  }
  const html = await grab(runs[0].run_id)
  ev.save('evidence.html', html)
  ev.check('with format html the saved evidence is a self-contained page naming the run and its workflow hash', html.startsWith('<!doctype html') && html.includes(runs[0].run_id) && html.includes(runs[0].workflow_hash), `${html.length} bytes`)
  ev.check('and with lineage off it points to the JSON form instead of embedding events', !/\"eventType\"/.test(html))
  await E2('evidence.set', { patch: { format: 'json', include_lineage: true }, idempotency_key: key() })
  const json = JSON.parse(await grab(runs[0].run_id))
  ev.save('evidence.json', json)
  ev.check('with format json it holds the run, the artifacts with digests and the lineage, and a document hash', json.run?.run_id === runs[0].run_id && (json.artifacts ?? []).length >= 5 && (json.lineage ?? []).length >= 1 && /^[0-9a-f]{64}$/.test(json.document_sha256 ?? ''), Object.keys(json).join(','))

  // 4. Storage: a plan, then a cleanup that removes real files. Runs look 40 days old under the hook.
  await app.stop()
  await sleep(2500)
  app = new App({ root, exe, port, env: { OGUN_STORAGE_CLOCK_OFFSET_DAYS: '40' } })
  await app.start()
  await app.waitEngine()
  const E3 = (m, p = {}) => app.engine(m, p)
  const store = join(root, 'home', 'profiles', 'studio', 'artifacts')
  const infoBefore = await E3('storage.info', {})
  const bytesBefore = dirBytes(store)
  ev.save('storage-info-before.json', infoBefore)
  const plan1 = await E3('storage.cleanup_plan', {})
  ev.save('cleanup-plan-keep-heads.json', plan1)
  ev.check('the plan counts what each rule would remove and keeps the newest output of every partition', (plan1.lines ?? []).length >= 1 && plan1.keeps_head !== false, JSON.stringify(plan1).slice(0, 300))
  await E3('storage.set_rules', { patch: { keep_lineage_referenced: false }, idempotency_key: key() })
  const plan2 = await E3('storage.cleanup_plan', {})
  ev.save('cleanup-plan.json', plan2)
  const planned = plan2.artifacts ?? (plan2.lines ?? []).reduce((n, l) => n + l.artifacts, 0)
  ev.check('without the keep rule the plan is larger and lists real artifacts', planned > (plan1.artifacts ?? 0) && planned >= 5, `${plan1.artifacts} -> ${planned} artifacts`)
  const after = await E3('storage.cleanup', {})
  ev.save('storage-info-after.json', after)
  const bytesAfter = dirBytes(store)
  ev.check('the cleanup removed the artifact bytes from disk', bytesAfter < bytesBefore, `${bytesBefore} -> ${bytesAfter} bytes under the artifact store`)
  const arts = (await app.invoke('artifacts_list', { params: { run_id: runs[0].run_id } })).artifacts ?? []
  ev.check('and the removed artifacts no longer list', arts.length === 0, `${arts.length} artifacts listed for the old run`)
  ev.check('the runs themselves stay (history is not deleted)', ((await app.invoke('runs_list', { params: {} })).runs ?? []).length >= 3)
  await app.goto('#/settings/storage')
  await sleep(1200)
  await app.shot(join(ev.dir, '09-storage.png'))
  await app.stop()
  return ev.finish()
}
