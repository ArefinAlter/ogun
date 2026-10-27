// Scenario 8: connections and exporters. A connection is made in the connection editor (settings only; secrets by
// name), its Test really does what a delivery would do, and a workflow delivers to a folder, a SQLite database and
// an S3-compatible bucket (signature-checked by a local server) after a person approved each delivery.
import { mkdirSync, readFileSync, readdirSync, statSync, existsSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'

function filesUnder(dir, out = []) {
  if (!existsSync(dir)) return out
  for (const n of readdirSync(dir)) {
    const p = join(dir, n)
    try {
      const st = statSync(p)
      if (st.isDirectory()) filesUnder(p, out)
      else if (st.size < 30_000_000) out.push(p)
    } catch {
      /* locked */
    }
  }
  return out
}

export async function run({ ev, root, cleanDir, launch, sleep, waitFor, http, workflow, key, FIXTURE_PORT }) {
  cleanDir(root)
  const { app } = await launch(root)
  await http(`http://127.0.0.1:${FIXTURE_PORT}/admin/reset`, { method: 'POST' })
  const exports = join(root, 'exports')
  mkdirSync(exports, { recursive: true })
  const dbFile = join(root, 'out.db')
  const S3_SECRET = 'acceptance-secret-ZEUS-CANARY-s3-0123456789'

  ev.check('no connection exists at first', ((await app.engine('connections.list', {})).connections ?? []).length === 0)
  // Secrets are entered once in the native process and kept in the credential store; a connection names them.
  await app.invoke('secret_put', { name: 'acc_s3_key', value: 'ACCEPTANCEKEYID' })
  await app.invoke('secret_put', { name: 'acc_s3_secret', value: S3_SECRET })
  await app.invoke('secret_put', { name: 'acc_s3_wrong', value: 'not-the-secret-just-a-wrong-one-0123456789' })
  const names = (await app.invoke('secrets_list')).secrets ?? (await app.invoke('secrets_list'))
  ev.save('secret-names.json', names)
  ev.check('the secret list holds names only', JSON.stringify(names).includes('acc_s3_secret') && !JSON.stringify(names).includes('ZEUS-CANARY'), JSON.stringify(names).slice(0, 200))

  const put = (c) => app.engine('connections.put', { connection: c, idempotency_key: key() })
  await put({ name: 'files-out', connector: 'local-files', settings: { directory: exports.replace(/\\/g, '/') } })
  await put({ name: 'db-out', connector: 'sqlite', settings: { path: dbFile.replace(/\\/g, '/') } })
  const minio = {
    connector: 'minio',
    auth: 'access_keys',
    settings: { endpoint: '127.0.0.1', port: FIXTURE_PORT, secure: false, bucket: 'acc', region: 'us-east-1' },
  }
  await put({ name: 'bucket', ...minio, secret_refs: { access_key_id: 'acc_s3_key', secret_access_key: 'acc_s3_secret' } })
  await put({ name: 'bucket-wrong', ...minio, secret_refs: { access_key_id: 'acc_s3_key', secret_access_key: 'acc_s3_wrong' } })
  // A secret value typed into a setting is refused, not masked.
  let refused = null
  try {
    await put({ name: 'leaky', connector: 'sqlite', settings: { path: 'sk-ant-api03-ZEUS-CANARY-0123456789abcdef' } })
  } catch (e) {
    refused = e.envelope
  }
  ev.check('a secret pasted into a setting is refused', !!refused && /secret/i.test(JSON.stringify(refused)), JSON.stringify(refused).slice(0, 200))
  const listed = (await app.engine('connections.list', {})).connections
  ev.save('connections.json', listed)
  ev.check('four connections are stored and show secret names only', listed.length === 4 && !JSON.stringify(listed).includes('ZEUS-CANARY'), `${listed.length}`)

  // Test really does something.
  const t = async (name) => app.engine('connections.test', { name, idempotency_key: key() }).then((v) => v, (e) => ({ error: e.envelope ?? String(e) }))
  const tFiles = await t('files-out')
  const tDb = await t('db-out')
  const tBucket = await t('bucket')
  const tWrong = await t('bucket-wrong')
  ev.save('tests.json', { tFiles, tDb, tBucket, tWrong })
  ev.check('Test of the folder connection passes (it wrote and removed a probe file)', tFiles.ok === true, JSON.stringify(tFiles).slice(0, 300))
  ev.check('Test of the SQLite connection passes (it opened the file and wrote a scratch table)', tDb.ok === true && existsSync(dbFile), JSON.stringify(tDb).slice(0, 300))
  ev.check('Test of the bucket connection passes against the signature-checking server', tBucket.ok === true, JSON.stringify(tBucket).slice(0, 300))
  ev.check('Test with a wrong secret fails, and says so without the secret', tWrong.ok === false && !JSON.stringify(tWrong).includes('wrong-one'), JSON.stringify(tWrong).slice(0, 300))
  const badSig = (await http(`http://127.0.0.1:${FIXTURE_PORT}/stats`)).json?.['s3-bad-signature'] ?? 0
  ev.check('the server saw the wrong signature (the test really sent a request)', badSig >= 1, `${badSig} bad signatures`)

  // The connection editor in the window.
  await app.goto('#/settings/connections')
  await sleep(1000)
  await app.shot(join(ev.dir, '08-connections.png'))
  const text = await app.page.locator('main').innerText()
  ev.check('the settings page lists the connections by name', /files-out/.test(text) && /db-out/.test(text) && /bucket/.test(text), text.slice(0, 160).replace(/\s+/g, ' '))

  // A workflow delivers; each delivery waits for a person.
  const put1 = await app.invoke('workflows_put', { params: { workflow: workflow('acc-export.workflow.json'), idempotency_key: key() } })
  ev.save('put.json', put1)
  const r = await app.invoke('runs_start', { params: { workflow: 'acc-export', partitions: { kind: 'latest' }, idempotency_key: key() } })
  const approvals = await waitFor(async () => {
    const a = ((await app.invoke('approvals_list', { params: { state: 'pending' } })).approvals ?? []).filter((x) => x.run_id === r.run_id)
    return a.length >= 3 ? a : null
  }, { ms: 90000, every: 1500, what: 'three delivery approvals' })
  ev.save('delivery-approvals.json', approvals)
  ev.check('three deliveries wait for a person, each with its own approval', approvals.length === 3, approvals.map((a) => a.node).join(','))
  const facts = JSON.stringify(approvals[0].facts)
  ev.check('the approval facts name the connection and where it delivers, not a secret', /files-out|db-out|bucket/.test(JSON.stringify(approvals)) && !facts.includes('ZEUS-CANARY'), facts.slice(0, 240))
  for (const a of approvals) {
    const p = await app.invoke('approvals_prepare', { approval_id: a.approval_id, decision: 'approve' })
    await sleep(p.arm_ms + 300)
    await app.invoke('approvals_confirm', { confirmation_id: p.confirmation_id })
  }
  const done = await waitFor(async () => {
    const x = await app.invoke('runs_get', { params: { run_id: r.run_id } })
    return ['succeeded', 'failed', 'cancelled'].includes(x.state) ? x : null
  }, { ms: 120000, every: 1500, what: 'the delivery run' })
  ev.save('run.json', done)
  ev.check('after the approvals the run succeeded', done.state === 'succeeded', JSON.stringify((done.nodes ?? []).map((n) => `${n.node}:${n.state}${n.error ? ' ' + JSON.stringify(n.error) : ''}`)))

  const delivered = join(exports, 'releases', 'rows.jsonl')
  const fileCandidates = filesUnder(exports)
  ev.save('exports-folder.txt', fileCandidates.join('\n'))
  ev.check('the folder received the rows as a file', fileCandidates.length >= 1 && readFileSync(fileCandidates[0], 'utf8').split('\n').filter(Boolean).length === 10, fileCandidates.join(','))
  const py = spawnSync('python', ['-c', `import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); print(c.execute('select count(*) from releases').fetchone()[0])`, dbFile], { encoding: 'utf8' })
  ev.check('the SQLite table holds the 10 rows', py.stdout.trim() === '10', (py.stdout + py.stderr).trim().slice(0, 200))
  const objs = (await http(`http://127.0.0.1:${FIXTURE_PORT}/s3-admin/objects`)).json ?? {}
  ev.save('s3-objects.json', objs)
  ev.check('the bucket received an object, and no further signature failed', Object.keys(objs).some((k) => k.startsWith('acc/releases/')) && ((await http(`http://127.0.0.1:${FIXTURE_PORT}/stats`)).json?.['s3-bad-signature'] ?? 0) === badSig, JSON.stringify(objs))
  void delivered

  // Secrets never leave the credential store: nothing the engine keeps contains them.
  const scan = filesUnder(join(root, 'home'))
  const hitsIn = scan.filter((f) => {
    try {
      const b = readFileSync(f)
      return b.includes('ZEUS-CANARY-s3') || b.includes('not-the-secret-just-a-wrong-one')
    } catch {
      return false
    }
  })
  ev.check('no file in the engine data (store, events, lineage, artifacts, logs, receipts) holds a secret value', hitsIn.length === 0, `${scan.length} files scanned; ${hitsIn.join(', ')}`)
  const events = await app.invoke('events_since', { params: { after: 0, limit: 1000, wait_ms: 250 } })
  ev.check('and neither do the run events', !JSON.stringify(events).includes('ZEUS-CANARY'))
  const lineage = await app.invoke('lineage_get', { params: { run_id: r.run_id } })
  ev.check('or the lineage', !JSON.stringify(lineage).includes('ZEUS-CANARY'))
  await app.stop()
  return ev.finish()
}
