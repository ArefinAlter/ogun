// Scenario 7: sign in to a hub from Studio desktop with the system browser flow (authorization code + PKCE, loopback
// redirect), tokens in the OS credential store, roles, refresh, refusal and sign-out. The hub is a real ogun-server
// with a real ogund; the identity provider is idp_mock.py (it behaves like Dex for a public client).
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { Hub, Browser, loginAtProvider } from '../hubstack.mjs'

function filesUnder(dir, out = []) {
  if (!existsSync(dir)) return out
  for (const n of readdirSync(dir)) {
    const p = join(dir, n)
    try {
      const st = statSync(p)
      if (st.isDirectory()) filesUnder(p, out)
      else if (st.size < 20_000_000) out.push(p)
    } catch {
      /* locked */
    }
  }
  return out
}

export async function run({ port, ev, exe, root, cleanDir, App, ps, sleep, waitFor, http, workflow, key, here }) {
  const bin = join(exe, '..')
  const appRoot = root + '-s7'
  cleanDir(appRoot)
  const hub = await new Hub({ root: root + '-hub', bin }).start()
  ev.note(`hub up at ${hub.base}, workspace ${hub.workspace}`)
  await http(`${hub.issuer}/admin/short-lived?seconds=3600`, { method: 'POST' })

  // The person enrols at the hub once, in a browser (an invitation is how an account is made).
  const aliceBrowser = await hub.enroll(hub.ownerLink, { login: 'alice@example.com', password: 'alice-password' })
  await hub.enroll(hub.viewerLink, { login: 'bob@example.com', password: 'bob-password' })
  ev.check('the owner and a viewer enrolled through the hub invitations', true)

  const urlFile = join(appRoot, 'signin-url.txt')
  const app = new App({ root: appRoot, exe, port, env: { OGUN_STUDIO_SIGNIN_URL_FILE: urlFile } })
  await app.start()
  await app.waitEngine()

  async function signIn(who, expectFail = false) {
    if (existsSync(urlFile)) writeFileSync(urlFile, '')
    const p = app.invoke('hub_signin_start', { url: hub.base, ca_pem: null }).then(
      (v) => ({ ok: v }),
      (e) => ({ err: e }),
    )
    const url = await waitFor(() => {
      const t = existsSync(urlFile) ? readFileSync(urlFile, 'utf8') : ''
      return t.startsWith('http') ? t : null
    }, { ms: 20000, what: 'the authorization URL' })
    const b = new Browser()
    const callback = await loginAtProvider(b, url, who)
    const page = await b.get(callback)
    const text = await page.text()
    const result = await p
    return { url, page: text, status: page.status, result }
  }

  // 1. Alice signs in.
  ev.check('no hub is known at first', (await app.invoke('hub_environments')).length === 0)
  const t0 = Date.now()
  const alice = await signIn({ login: 'alice@example.com', password: 'alice-password' })
  ev.save('alice-signin-url.txt', alice.url.replace(/(state|nonce|code_challenge)=[^&]+/g, '$1=<redacted>'))
  const u = new URL(alice.url)
  ev.check('the browser was sent to the provider with PKCE S256 and a loopback redirect', u.searchParams.get('code_challenge_method') === 'S256' && /^http:\/\/127\.0\.0\.1:\d+\/callback$/.test(u.searchParams.get('redirect_uri')) && u.searchParams.get('response_type') === 'code', u.origin + u.pathname)
  ev.check('as the public client: no client secret anywhere in the URL', u.searchParams.get('client_id') === 'ogun-studio' && !/secret/i.test(alice.url))
  ev.check('the app shows the person a short page and then needs nothing else', alice.status === 200 && /Signed in/.test(alice.page), alice.page.slice(0, 120))
  ev.check('the sign-in completed', !alice.result.err && alice.result.ok?.signed_in === true, JSON.stringify(alice.result).slice(0, 300))
  const view = alice.result.ok
  ev.save('alice-view.json', view)
  ev.check('the hub says who this is and what role they have', view?.user === 'Alice Example' && view?.role === 'owner', `${view?.user} / ${view?.role} / ${view?.workspace}`)
  ev.note(`sign-in took ${Date.now() - t0} ms`)
  const listed = await app.invoke('hub_environments')
  ev.check('the hub is listed as signed in with browser sign-in', listed.length === 1 && listed[0].auth === 'oidc' && listed[0].signed_in, JSON.stringify(listed).slice(0, 300))

  // Where the tokens are, and where they are not.
  const creds = ps('cmdkey /list')
  const mine = creds.split('\n').filter((l) => /Ogun Studio hubs/i.test(l))
  ev.save('credential-store-targets.txt', mine.join('\n'))
  ev.check('the session and refresh tokens are in the Windows credential store', mine.length >= 2, `${mine.length} entries named Ogun Studio hubs`)
  const scanned = [...filesUnder(join(appRoot, 'shell')), ...filesUnder(join(appRoot, 'home')), ...filesUnder(join(appRoot, 'webview'))]
  const leaks = scanned.filter((f) => {
    try {
      return readFileSync(f).includes('ogs1_')
    } catch {
      return false
    }
  })
  ev.check('no file of the app, its engine or its webview holds a session token', leaks.length === 0, `${scanned.length} files scanned; ${leaks.join(', ')}`)
  const hubsJson = readFileSync(join(appRoot, 'shell', 'hubs', 'hubs.json'), 'utf8')
  ev.check('the saved hub list holds no token', !/token|ogs1_|refresh/i.test(hubsJson), hubsJson.slice(0, 200))

  // 2. Use the hub: reads, and what the owner role allows.
  const info = await app.invoke('hub_use', { id: view.id })
  ev.save('hub-info-owner.json', info)
  ev.check('the link is the hub, as a person', info.link === 'hub' && info.hub_user === 'Alice Example' && info.hub_role === 'owner' && info.connected, JSON.stringify(info).slice(0, 300))
  ev.check('Studio on a hub cannot decide approvals', info.can_decide === false)
  const seed = (name) => {
    const r = hub.run('ogun.exe', ['--profile', 'server', 'put', join(here, 'workflows', name)])
    return r
  }
  const put = seed('acc-local-fixture.workflow.json')
  ev.check('a workflow was stored on the hub by its operator (CLI)', put.status === 0, (put.stdout + put.stderr).slice(0, 200))
  const wfs = await app.invoke('workflows_list')
  ev.check('the hub link lists the hub workflow', (wfs.workflows ?? []).some((w) => w.name === 'acc-local-fixture'), JSON.stringify(wfs).slice(0, 200))
  const started = await app.invoke('runs_start', { params: { workflow: 'acc-local-fixture', partitions: { kind: 'latest' }, idempotency_key: key() } })
  const run = await waitFor(async () => {
    const r = await app.invoke('runs_get', { params: { run_id: started.run_id } })
    return ['succeeded', 'failed', 'cancelled'].includes(r.state) ? r : null
  }, { ms: 90000, what: 'the hub run' })
  ev.check('an owner started a run on the hub from Studio and it succeeded', run.state === 'succeeded', run.state)

  // Approvals on the hub: seed one, then try to decide it every way a client could.
  const repo = join(here, '..', '..')
  const code = hub.run('ogun.exe', ['--profile', 'server', 'put', join(repo, 'examples', 'bikeshare', 'rebalance-plan.workflow.json')], undefined, repo)
  ev.save('hub-put-code-workflow.txt', (code.stdout + code.stderr).slice(0, 600))
  const approvals = await app.invoke('approvals_list', { params: { state: 'pending' } })
  const pending = approvals.approvals ?? []
  ev.check('a pending approval exists on the hub and Studio can read it', pending.length >= 1, `${pending.length} pending`)
  let refused = null
  try {
    await app.invoke('approvals_prepare', { approval_id: pending[0].approval_id, decision: 'approve' })
  } catch (e) {
    refused = e.envelope ?? { message: String(e) }
  }
  ev.check('Studio refuses to decide a hub approval', !!refused, JSON.stringify(refused).slice(0, 200))
  // An API token is an agent: it reads, and the decision route is closed to it.
  const me = await (await aliceBrowser.get(`${hub.base}/api/v1/me`)).json()
  const csrf = me.csrf_token
  const minted = await aliceBrowser.request(`${hub.base}/api/v1/tokens`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-ogun-csrf': csrf, origin: hub.base },
    body: JSON.stringify({ name: 'agent', role: 'editor' }),
  })
  const token = (await minted.json()).secret
  const agent = (path, init = {}) => http(hub.base + path, { ...init, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json', ...(init.headers ?? {}) } })
  const agentList = await agent('/api/v1/approvals')
  const agentDecide = await agent(`/api/v1/approvals/${pending[0].approval_id}/decide`, { method: 'POST', body: JSON.stringify({ decision: 'approve' }) })
  ev.save('agent-token-approval-attempts.json', { list: agentList.status, decide: agentDecide.status, body: agentDecide.json })
  ev.check('an API token (an agent) can read approvals', agentList.status === 200, `${agentList.status}`)
  ev.check('an API token cannot decide one', agentDecide.status === 403, `${agentDecide.status} ${agentDecide.text.slice(0, 160)}`)

  // 3. A viewer on the same hub: reads, and the hub refuses what the role does not allow.
  await app.invoke('hub_signout', { id: view.id })
  const afterOut = await app.invoke('hub_environments')
  ev.check('signing out keeps the hub in the list as signed out', afterOut.length === 1 && afterOut[0].signed_in === false, JSON.stringify(afterOut).slice(0, 200))
  const credsAfter = ps('cmdkey /list').split('\n').filter((l) => /Ogun Studio hubs/i.test(l))
  ev.check('signing out removed the tokens from the credential store', credsAfter.length === 0, `${credsAfter.length} left`)
  const log = readFileSync(join(hub.root, 'ogun-server.log'), 'utf8')
  ev.check('the hub ended the session (its log says so)', /signed out/.test(log))
  ev.check('and logged the native sign-in by user id, not by token', /signed in natively/.test(log) && !/ogs1_/.test(log))
  const bob = await signIn({ login: 'bob@example.com', password: 'bob-password' })
  ev.check('a viewer signs in', bob.result.ok?.role === 'viewer', JSON.stringify(bob.result).slice(0, 200))
  await app.invoke('hub_use', { id: bob.result.ok.id })
  let bobDenied = null
  try {
    await app.invoke('runs_start', { params: { workflow: 'acc-local-fixture', partitions: { kind: 'latest' }, idempotency_key: key() } })
  } catch (e) {
    bobDenied = e.envelope ?? { message: String(e) }
  }
  ev.check('the hub refuses a viewer who tries to start a run', bobDenied && /permission|role/i.test(JSON.stringify(bobDenied)), JSON.stringify(bobDenied).slice(0, 200))
  ev.check('but the viewer can read', ((await app.invoke('runs_list', { params: {} })).runs ?? []).length >= 1)
  await app.shot(join(ev.dir, '07-hub-viewer.png'))

  // 4. An account the hub does not have.
  await app.invoke('hub_signout', { id: bob.result.ok.id }).catch(() => {})
  const mallory = await signIn({ login: 'mallory@example.com', password: 'mallory-password' })
  ev.check('an identity with no account on the hub is refused, with a way forward', !!mallory.result.err && /no account|invitation/i.test(JSON.stringify(mallory.result.err)), JSON.stringify(mallory.result.err).slice(0, 240))

  // 5. Refresh: the hub's session ends on its idle timeout (one minute here); the app signs in again by itself.
  const again = await signIn({ login: 'alice@example.com', password: 'alice-password' })
  await app.invoke('hub_use', { id: again.result.ok.id })
  await app.invoke('workflows_list')
  ev.note('waiting 75 s for the hub session to go idle')
  await sleep(75000)
  const idpBefore = (await http(`${hub.issuer}/admin/log`, { method: 'POST' })).json?.log ?? []
  const refreshed = await app.invoke('workflows_list').then((v) => ({ ok: v }), (e) => ({ err: e.envelope ?? String(e) }))
  const idpAfter = (await http(`${hub.issuer}/admin/log`, { method: 'POST' })).json?.log ?? []
  ev.check('after the session ended the next call worked: the app refreshed and signed in again', !refreshed.err && (refreshed.ok.workflows ?? []).length >= 1, JSON.stringify(refreshed).slice(0, 200))
  ev.save('idp-requests-around-refresh.json', { before: idpBefore.length, after: idpAfter.length, tail: idpAfter.slice(-6) })
  ev.check('the provider saw a refresh-token grant', idpAfter.length > idpBefore.length)
  const hubLog = readFileSync(join(hub.root, 'ogun-server.log'), 'utf8')
  ev.check('the hub logged a second native sign-in', (hubLog.match(/signed in natively/g) ?? []).length >= 4, `${(hubLog.match(/signed in natively/g) ?? []).length}`)

  // 6. The provider stops honouring the refresh token: the app says signed out, and says it plainly.
  await http(`${hub.issuer}/admin/revoke-refresh`, { method: 'POST' })
  ev.note('waiting 75 s again; the refresh token has been revoked at the provider')
  await sleep(75000)
  const refused2 = await app.invoke('workflows_list').then((v) => ({ ok: v }), (e) => ({ err: e.envelope ?? String(e) }))
  ev.check('with the refresh token revoked the call fails as unauthenticated', refused2.err && /unauthenticated|sign in/i.test(JSON.stringify(refused2.err)), JSON.stringify(refused2).slice(0, 240))
  const envs = await app.invoke('hub_environments')
  ev.check('and the hub shows as signed out', envs.length === 1 && envs[0].signed_in === false, JSON.stringify(envs).slice(0, 200))
  await app.shot(join(ev.dir, '08-signed-out.png'))
  // Sign in again and remove the hub.
  const third = await signIn({ login: 'alice@example.com', password: 'alice-password' })
  ev.check('signing in again works', third.result.ok?.signed_in === true)
  await app.invoke('hub_signout', { id: third.result.ok.id })
  await app.invoke('hub_signout', { id: third.result.ok.id })
  ev.check('signing out of a signed-out hub removes it from the list', (await app.invoke('hub_environments')).length === 0)
  await app.stop(true)
  await hub.stop()
  return ev.finish()
}
