// Scenario 5: a code operator (Python, from the interpreter Studio ships) runs in the Windows sandbox only after a
// person approved the exact code, and from inside it cannot list the person's profile, write outside its scratch
// folder, read a file nobody granted, or reach the network (a loopback listener the harness opened).
import { createServer } from 'node:net'
import { mkdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'

export async function run({ ev, exe, root, here, cleanDir, launch, sleep, waitFor, workflow, key }) {
  cleanDir(root)
  const { app } = await launch(root)
  const status = await app.engine('sandbox.status', {})
  ev.save('sandbox-status.json', status)
  ev.check('the Windows sandbox is available and says what it does and does not isolate', status.available === true && status.backend === 'appcontainer' && status.isolates?.length >= 4 && status.does_not_isolate?.length >= 3, `${status.backend}`)

  // The things the operator will try, set up outside its reach.
  const lab = join(root, 'lab')
  mkdirSync(join(lab, 'outside'), { recursive: true })
  writeFileSync(join(lab, 'secret.txt'), 'ZEUS-CANARY-not-for-operators')
  let hits = 0
  const listener = createServer((s) => {
    hits++
    s.destroy()
  })
  await new Promise((r) => listener.listen(0, '127.0.0.1', r))
  const port = listener.address().port
  const profile = process.env.USERPROFILE

  // Store the workflow with its code; the code hash is the file's BLAKE3.
  const codePath = join(here, 'ops', 'summarize.py')
  const hashed = spawnSync(join(exe, '..', 'ogun.exe'), ['hash-file', codePath], { encoding: 'utf8' }).stdout.trim()
  const wf = workflow('acc-python.workflow.json')
  wf.nodes.find((n) => n.id === 'summary').code.blake3 = hashed
  wf.nodes.find((n) => n.id === 'summary').config = {
    probe_dir: profile,
    outside_dir: join(lab, 'outside'),
    secret_file: join(lab, 'secret.txt'),
    probe_addr: `127.0.0.1:${port}`,
  }
  const put = await app.invoke('workflows_put', {
    params: { workflow: wf, idempotency_key: key(), code: { [hashed]: readFileSync(codePath).toString('base64') } },
  })
  ev.save('put.json', put)
  ev.check('the workflow is stored but waits for approval of its code', (put.approvals ?? []).length === 1, JSON.stringify(put.approvals).slice(0, 200))
  let refusedRun = null
  try {
    await app.invoke('runs_start', { params: { workflow: 'acc-python', partitions: { kind: 'latest' }, idempotency_key: key() } })
  } catch (e) {
    refusedRun = e.envelope
  }
  ev.check('before approval the run does not start', !!refusedRun, JSON.stringify(refusedRun).slice(0, 200))

  const appr = (await app.invoke('approvals_list', { params: { state: 'pending' } })).approvals
  const mine = appr.find((a) => a.workflow === 'acc-python')
  const facts = await app.invoke('approvals_get', { params: { approval_id: mine.approval_id } })
  ev.save('approval-facts.json', facts)
  const p = await app.invoke('approvals_prepare', { approval_id: mine.approval_id, decision: 'approve' })
  await sleep(p.arm_ms + 300)
  const decided = await app.invoke('approvals_confirm', { confirmation_id: p.confirmation_id })
  ev.check('a person approved exactly that code in Studio', decided.state === 'approved' && decided.decided_via === 'studio', `${decided.state} via ${decided.decided_via}`)

  const r = await app.invoke('runs_start', { params: { workflow: 'acc-python', partitions: { kind: 'latest' }, idempotency_key: key() } })
  const finished = await waitFor(async () => {
    const x = await app.invoke('runs_get', { params: { run_id: r.run_id } })
    return ['succeeded', 'failed', 'cancelled'].includes(x.state) ? x : null
  }, { ms: 180000, every: 1500, what: 'the code run' })
  ev.save('run.json', finished)
  ev.check('the code operator ran to success inside the sandbox', finished.state === 'succeeded', JSON.stringify((finished.nodes ?? []).map((n) => `${n.node}:${n.state}${n.error ? ' ' + JSON.stringify(n.error) : ''}`)))
  const arts = (await app.invoke('artifacts_list', { params: { run_id: r.run_id } })).artifacts ?? []
  const out = arts.find((a) => a.node === 'summary')
  let rows = []
  if (out) {
    const g = await app.invoke('artifacts_get', { params: { blake3: out.blake3, inline_max_bytes: 100000 } })
    const t = Buffer.from(g.content_base64 ?? '', 'base64').toString('utf8')
    writeFileSync(join(ev.dir, 'operator-output.jsonl'), t)
    rows = t.split('\n').filter(Boolean).map((l) => JSON.parse(l))
  }
  const byName = Object.fromEntries(rows.filter((x) => x.check).map((x) => [x.check, x.result]))
  ev.check('it read its input rows (10) with the bundled Python', rows[0]?.rows === 10 && /^3\./.test(rows[0]?.python ?? ''), JSON.stringify(rows[0]))
  ev.check('it wrote in its scratch folder', byName.write_in_scratch === 'allowed', JSON.stringify(byName))
  ev.check('it could not list the person profile folder', byName.list_the_person_profile === 'blocked', JSON.stringify(byName))
  ev.check('it could not write a file next to the secret', byName.write_outside_scratch === 'blocked' && !existsSync(join(lab, 'outside', 'planted.txt')), JSON.stringify(byName))
  ev.check('it could not read a file nobody granted', byName.read_a_file_nobody_granted === 'blocked', JSON.stringify(byName))
  ev.check('it could not reach a loopback listener', byName.connect_to_loopback === 'blocked' && hits === 0, `listener hits ${hits}`)
  const outText = JSON.stringify(rows)
  ev.check('the canary never appears in the operator output', !outText.includes('ZEUS-CANARY'))
  // The same operator with a changed file is a different hash: the approval does not carry over.
  const changed = Buffer.from(readFileSync(codePath).toString('utf8') + '\n# changed\n')
  const hash2 = (() => {
    const tmp = join(ev.dir, 'changed.py')
    writeFileSync(tmp, changed)
    return spawnSync(join(exe, '..', 'ogun.exe'), ['hash-file', tmp], { encoding: 'utf8' }).stdout.trim()
  })()
  const wf2 = JSON.parse(JSON.stringify(wf))
  wf2.version = 2
  wf2.nodes.find((n) => n.id === 'summary').code.blake3 = hash2
  const put2 = await app.invoke('workflows_put', { params: { workflow: wf2, idempotency_key: key(), code: { [hash2]: changed.toString('base64') } } })
  ev.check('changing the code makes a new version that needs a new approval', (put2.approvals ?? []).length === 1 && put2.workflow_hash !== put.workflow_hash, `${put2.approvals?.length} approvals`)
  listener.close()
  await app.shot(join(ev.dir, '05-after-code-run.png'))
  await app.stop()
  return ev.finish()
}
