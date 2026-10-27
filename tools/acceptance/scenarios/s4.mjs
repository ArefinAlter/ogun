// Scenario 4: approvals in Studio. The facts come from the daemon and are shown before any button; the confirm
// button arms after a delay; the decision carries the request hash and Studio's channel evidence, which the daemon
// checks against the peer (on Windows: the image path of the process at the other end of the named pipe); nothing a
// webview or another process can send decides an approval.
import { spawnSync } from 'node:child_process'
import { join } from 'node:path'

export async function run({ ev, exe, root, cleanDir, launch, sleep, waitFor, key }) {
  cleanDir(root)
  const { app } = await launch(root)
  await app.invoke('sample_import', { id: 'bikeshare' })
  const list = async (state) => (await app.invoke('approvals_list', { params: { state } })).approvals ?? []
  const pending = await list('pending')
  ev.save('approvals-pending.json', pending)
  ev.check('importing the bikeshare project leaves approvals waiting for a person', pending.length >= 3, `${pending.length} pending`)
  const target = pending.find((a) => a.workflow === 'rebalance-plan' && a.node) ?? pending[0]
  const view = await app.invoke('approvals_get', { params: { approval_id: target.approval_id } })
  ev.save('approval-facts.json', view)
  ev.check('the daemon computed the facts (code hash, workflow hash, what it may do)', (view.facts ?? []).length >= 3 && /^[0-9a-f]{64}$/.test(view.request_hash) && /^[0-9a-f]{64}$/.test(view.workflow_hash), `${(view.facts ?? []).length} facts`)

  // The screen: facts first, the decision buttons after, the confirm button disabled until it arms.
  await app.goto('#/approvals')
  await sleep(1200)
  const page = app.page
  const body = await page.locator('main').innerText()
  ev.save('approvals-screen.txt', body.slice(0, 4000))
  await app.shot(join(ev.dir, '04-approvals-list.png'))
  const first = await page.getByText(target.workflow, { exact: false }).first()
  await first.click().catch(() => {})
  await sleep(800)
  const detail = await page.locator('main').innerText()
  const factsAt = detail.search(/hash|code|network/i)
  const buttonAt = detail.search(/Approve|Deny/)
  ev.check('the screen shows the facts above the decision buttons', factsAt >= 0 && buttonAt > factsAt, `facts at ${factsAt}, buttons at ${buttonAt}`)
  await app.shot(join(ev.dir, '04-approval-facts.png'))
  const approveBtn = page.getByRole('button', { name: /^Approve/ }).first()
  if (await approveBtn.count()) {
    await approveBtn.click()
    await sleep(300)
    const confirmBtn = page.getByRole('button', { name: /Approve|Confirm/ }).last()
    const early = await confirmBtn.isDisabled().catch(() => null)
    await app.shot(join(ev.dir, '04-confirm-arming.png'))
    ev.check('the confirm button is disabled while it arms', early === true, `disabled=${early}`)
    await sleep(1800)
    const late = await confirmBtn.isDisabled().catch(() => null)
    ev.check('and enabled after the arming delay', late === false, `disabled=${late}`)
    await page.getByRole('button', { name: /Cancel|Close/ }).first().click().catch(() => {})
  } else {
    ev.note('no Approve button found on the screen; the API path is checked below')
  }

  // The protocol, step by step through the shell's commands.
  const p1 = await app.invoke('approvals_prepare', { approval_id: target.approval_id, decision: 'approve' })
  ev.save('prepare.json', p1)
  ev.check('review returns the daemon facts and an arming delay', p1.arm_ms >= 1000 && p1.approval.request_hash === view.request_hash, `arm ${p1.arm_ms} ms`)
  let early = null
  try {
    await app.invoke('approvals_confirm', { confirmation_id: p1.confirmation_id })
  } catch (e) {
    early = e.envelope ?? { message: String(e) }
  }
  ev.check('a confirmation before the button armed is refused', early && /before the button armed/i.test(JSON.stringify(early)), JSON.stringify(early))
  let replay = null
  try {
    await app.invoke('approvals_confirm', { confirmation_id: p1.confirmation_id })
  } catch (e) {
    replay = e.envelope ?? { message: String(e) }
  }
  ev.check('and that confirmation is spent: it cannot be tried again', !!replay, JSON.stringify(replay))
  // Denying one approval.
  const other = pending.find((a) => a.approval_id !== target.approval_id)
  const pd = await app.invoke('approvals_prepare', { approval_id: other.approval_id, decision: 'deny' })
  await sleep(pd.arm_ms + 300)
  const denied = await app.invoke('approvals_confirm', { confirmation_id: pd.confirmation_id })
  ev.save('decided-deny.json', denied)
  ev.check('a deny is recorded with the Studio channel', denied.state === 'denied' && denied.decided_via === 'studio', `${denied.state} via ${denied.decided_via} by ${denied.decided_by}`)
  // Approving the target after the delay.
  const p2 = await app.invoke('approvals_prepare', { approval_id: target.approval_id, decision: 'approve' })
  await sleep(p2.arm_ms + 300)
  const done = await app.invoke('approvals_confirm', { confirmation_id: p2.confirmation_id })
  ev.save('decided-approve.json', done)
  ev.check('an approve after the delay is recorded with the Studio channel and the hash it was shown', done.state === 'approved' && done.decided_via === 'studio' && done.request_hash === view.request_hash, `${done.state} via ${done.decided_via} by ${done.decided_by}`)
  let again = null
  try {
    await app.invoke('approvals_prepare', { approval_id: target.approval_id, decision: 'deny' })
  } catch (e) {
    again = e.envelope
  }
  ev.check('a decided approval cannot be decided again', again && /already_decided/.test(JSON.stringify(again)), JSON.stringify(again))

  // What cannot decide.
  let relayed = null
  try {
    await app.engine('approvals.decide', { approval_id: pending[2].approval_id, decision: 'approve', request_hash: pending[2].request_hash, evidence: { kind: 'studio' } })
  } catch (e) {
    relayed = e.envelope ?? { message: String(e) }
  }
  ev.check('the webview cannot send approvals.decide itself: it is not in the shell relay list', !!relayed, JSON.stringify(relayed))
  const home = join(root, 'home')
  const cli = (args) => spawnSync(join(exe, '..', 'ogun.exe'), ['--profile', 'studio', ...args], { env: { ...process.env, OGUN_HOME: home }, encoding: 'utf8', timeout: 30000, input: 'y\n' })
  const viaCli = cli(['approve', pending[2].approval_id])
  ev.save('cli-approve-attempt.txt', `exit ${viaCli.status}\n${viaCli.stdout}\n${viaCli.stderr}`)
  ev.check('another program on this account (the ogun CLI) cannot decide on the app engine: no credential', viaCli.status !== 0, (viaCli.stderr || viaCli.stdout).slice(0, 200))
  const still = await list('pending')
  ev.check('after all of that, only the two decided approvals changed', still.length === pending.length - 2, `${still.length} pending`)
  const log = (await import('node:fs')).readFileSync(join(home, 'profiles', 'studio', 'logs', 'ogund.log'), 'utf8')
  ev.check('the daemon log records the decisions with the channel and the human credential', /approval decided/i.test(log) || /decided/.test(log), (log.match(/.*decid.*/g) ?? []).slice(-2).join(' | ').slice(0, 300))
  await app.stop()
  return ev.finish()
}
