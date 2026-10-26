import { useEffect, useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { BridgeView, EngineInfo, EnvironmentView, HubEnvironment, OgunError } from '../../api/types'
import { fmtAgo, fmtTime, idempotencyKey } from '../../lib/format'
import { useAsync, useBackend, useInfo } from '../../lib/hooks'
import { useScope } from '../../lib/scope'
import { Chip, CodeCopy, Dialog, ErrorBox, Kv, Loading, Notice, PageHead, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { ScopeBar } from '../../ui/ScopeBar'

function bridgeChip(b: BridgeView) {
  if (b.state === 'running') return <Chip tone="ok">running</Chip>
  if (b.state === 'locked') return <Chip>locked</Chip>
  if (b.state === 'stopped') return <Chip tone="bad">stopped</Chip>
  return <Chip>not configured</Chip>
}

function engineChip(e: EngineInfo) {
  switch (e.state) {
    case 'running':
      return <Chip tone="ok">running</Chip>
    case 'starting':
      return <Chip tone="warn">starting</Chip>
    case 'restarting':
      return <Chip tone="warn">restarting, attempt {e.attempt ?? 1}</Chip>
    case 'failed':
      return <Chip tone="bad">failed</Chip>
    case 'stopped':
      return <Chip>stopped</Chip>
    default:
      return <Chip>a daemon you started</Chip>
  }
}

/** Studio's own engine: the default environment of the desktop app (ADR 0015). */
function EngineGroup({ engine, onChanged }: { engine: EngineInfo; onChanged: () => void }) {
  const backend = useBackend()
  const prefs = useAsync(() => (backend.engine ? backend.engine.prefs() : Promise.resolve(null)), [backend])
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  const restart = async () => {
    setBusy(true)
    setError(undefined)
    try {
      await backend.engine?.restart()
      onChanged()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const setTray = async (on: boolean) => {
    setError(undefined)
    try {
      await backend.engine?.setPrefs({ keep_in_tray: on })
      prefs.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  return (
    <SettingsGroup title={<>Local engine {engineChip(engine)}</>} note="Ogun Studio runs its own engine (ogund) on this computer, started with Studio and stopped with it. Its data stays in the state folder above, and uninstalling Studio does not delete it.">
      <ErrorBox error={error} title="Engine" />
      <SettingRow title="State" desc={engine.state === 'running' ? `Process ${engine.pid ?? '?'}${engine.version ? `, version ${engine.version}` : ''}. Restarted ${engine.restarts} time${engine.restarts === 1 ? '' : 's'} since Studio opened.` : (engine.error ?? 'Not running.')}>
        <button type="button" class="btn small" disabled={busy || engine.state === 'external'} onClick={() => void restart()}>
          Restart the engine
        </button>
      </SettingRow>
      <SettingRow title="Keep running in the tray" desc="Closing the window hides Studio to the tray and keeps the engine running, so schedules keep firing. Quit from the tray menu to stop it. Off: closing the window stops the engine and its schedules.">
        <Switch label="Keep Studio running in the tray when I close the window (schedules keep firing)" checked={!!prefs.data?.keep_in_tray} disabled={!prefs.data} onChange={(on) => void setTray(on)} />
      </SettingRow>
      <SettingRow title="Network access" desc="Limited to this computer. The engine listens only on a local pipe or socket, with a credential that exists only while Studio runs." />
      <SettingRow title="State folder" desc={<span class="mono">{engine.state_dir}</span>} />
      <SettingRow title="Log" desc={<span class="mono">{engine.log_path}</span>} />
    </SettingsGroup>
  )
}

/** Hubs, optional: signed in to with a browser, or reached with a read-only token. */
function HubsGroup({ onAdd, onChanged }: { onAdd: () => void; onChanged: () => void }) {
  const backend = useBackend()
  const { info } = useInfo()
  const hubs = useAsync(() => (backend.hubs ? backend.hubs.list() : Promise.resolve([] as HubEnvironment[])), [backend, info?.link])
  const [error, setError] = useState<OgunError | undefined>()
  const act = async (f: () => Promise<unknown>) => {
    setError(undefined)
    try {
      await f()
      hubs.reload()
      onChanged()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  const saved = info?.link === 'hub' && info.hubUrl ? info.hubUrl : null
  const list = hubs.data ?? []
  return (
    <SettingsGroup title="Hubs" note="A hub is an Ogun server your team runs. It is optional: Studio works fully on its own engine. On a hub, what you may do is decided by the hub's own roles, and agents on a hub can never decide an approval.">
      <ErrorBox error={error} title="Hub" />
      {hubs.error ? (
        <div class="row">
          <div class="text">
            <div class="desc">Signing in to hubs is not available in this build: {hubs.error.message}</div>
          </div>
        </div>
      ) : null}
      {saved && list.length === 0 ? (
        <SettingRow title={<>{saved} <Chip tone="ok">in use</Chip> <Chip>API token, read-only</Chip></>} desc="This hub was added with a token.">
          <button type="button" class="btn small" onClick={() => void act(async () => void (await backend.linkLocal?.().then(onChanged)))}>
            Use the local engine
          </button>
        </SettingRow>
      ) : null}
      {list.map((h) => (
        <SettingRow
          key={h.id}
          title={
            <>
              {h.name} {h.active ? <Chip tone="ok">in use</Chip> : null} <Chip>{h.auth === 'oidc' ? 'browser sign-in' : 'API token, read-only'}</Chip> {h.auth === 'oidc' ? (h.signed_in ? <Chip tone="ok">signed in</Chip> : <Chip tone="warn">signed out</Chip>) : null}
            </>
          }
          desc={[h.url, h.user ? `signed in as ${h.user}` : ''].filter(Boolean).join(' · ')}
        >
          <span class="inline">
            {h.active ? (
              <button type="button" class="btn small" onClick={() => void act(async () => void (await backend.linkLocal?.()))}>
                Use the local engine
              </button>
            ) : (
              <button type="button" class="btn small" disabled={h.auth === 'oidc' && !h.signed_in} onClick={() => void act(async () => void (await backend.hubs?.use(h.id)))}>
                Use this hub
              </button>
            )}
            {h.auth === 'oidc' && h.signed_in ? (
              <button type="button" class="btn small ghost" onClick={() => void act(() => backend.hubs?.signOut(h.id) ?? Promise.resolve())}>
                Sign out
              </button>
            ) : null}
          </span>
        </SettingRow>
      ))}
      {list.length === 0 && !saved && !hubs.error ? (
        <div class="row">
          <div class="text">
            <div class="desc">No hub. Add one to look at a team server from this Studio.</div>
          </div>
        </div>
      ) : null}
      <div class="row">
        <div class="text" />
        <div class="control">
          <button type="button" class="btn small" onClick={onAdd}>
            <Icon name="plus" /> Add hub
          </button>
        </div>
      </div>
    </SettingsGroup>
  )
}

function AddHub({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const backend = useBackend()
  const [url, setUrl] = useState('')
  const [token, setToken] = useState('')
  const [ca, setCa] = useState('')
  const [remember, setRemember] = useState(true)
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  const [waiting, setWaiting] = useState(false)
  const signIn = async () => {
    setWaiting(true)
    setError(undefined)
    try {
      await backend.hubs?.signIn(url, ca.trim() ? ca : null)
      onDone()
      onClose()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setWaiting(false)
    }
  }
  const connect = async () => {
    setBusy(true)
    setError(undefined)
    try {
      await backend.linkHub?.(url, token, ca.trim() ? ca : null, remember)
      setToken('')
      onDone()
      onClose()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Dialog title="Add a hub" onClose={onClose}>
      {backend.linkHub ? (
        <>
          <p class="muted">A hub is an Ogun server your team runs (ogun-server). It is optional: Studio works fully on its own engine.</p>
          <label class="field">
            Hub URL
            <input value={url} placeholder="https://hub.example.org" size={44} onInput={(e) => setUrl((e.target as HTMLInputElement).value)} />
          </label>
          <label class="field">
            CA certificate (PEM), only for a hub with a private CA
            <textarea value={ca} style={{ minHeight: '56px' }} onInput={(e) => setCa((e.target as HTMLTextAreaElement).value)} />
          </label>
          {backend.hubs ? (
            <>
              <h3 style={{ margin: '14px 0 4px' }}>Sign in with your browser</h3>
              <p class="muted">Studio opens the hub's sign-in page in your browser (authorization code with PKCE) and never sees your password. The tokens are kept in your operating system's credential store and refreshed for you. On the hub you act with your own roles; an agent on a hub can never decide an approval.</p>
              <div class="toolbar">
                <button type="button" class="btn primary" disabled={waiting || busy || !url} onClick={() => void signIn()}>
                  {waiting ? 'Waiting for the browser…' : 'Sign in with your browser'}
                </button>
              </div>
              <hr style={{ margin: '14px 0' }} />
            </>
          ) : null}
          <h3 style={{ margin: '0 0 4px' }}>{backend.hubs ? 'Or use an API token (read-only)' : 'Use an API token (read-only)'}</h3>
          <p class="muted">An API token acts as an agent: Studio can read the hub and can never decide an approval with it. The token stays in Studio's native process; this page never reads it back.</p>
          <label class="field">
            API token
            <input type="password" autocomplete="off" value={token} size={44} onInput={(e) => setToken((e.target as HTMLInputElement).value)} />
          </label>
          <label class="check" style={{ margin: '8px 0' }}>
            <input type="checkbox" checked={remember} onChange={(e) => setRemember((e.target as HTMLInputElement).checked)} />
            remember this hub on this device
          </label>
          <ErrorBox error={error} />
          <div class="toolbar">
            <button type="button" class="btn" onClick={onClose}>
              Cancel
            </button>
            <button type="button" class="btn" disabled={busy || waiting || !url || !token} onClick={() => void connect()}>
              Connect with the token
            </button>
          </div>
        </>
      ) : (
        <>
          <p>
            Only the desktop app links to a hub. {backend.demo ? 'This build is not linked to a hub.' : 'Studio-web is served by the hub itself, so it is already there.'} On the hub, a person with a browser session creates the API token; it acts as an agent.
          </p>
          <div class="toolbar">
            <button type="button" class="btn" onClick={onClose}>
              Close
            </button>
          </div>
        </>
      )}
    </Dialog>
  )
}

export function EnvironmentsSettings() {
  const backend = useBackend()
  const { info, refresh } = useInfo()
  const scope = useScope()
  const envs = useAsync(() => (backend.environments ? backend.environments() : Promise.resolve(null)), [backend])
  const creds = useAsync(() => (info?.canReadSettings ? backend.credentialsList() : Promise.resolve(null)), [backend, info?.canReadSettings])
  const [clients, setClients] = useState(false)
  const [adding, setAdding] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  const e = envs.data
  const local = e?.environments.find((x) => x.kind === 'local')
  const others = (e?.environments ?? []).filter((x) => x.kind !== 'local' && (!scope.environment || x.id === scope.environment))
  const showLocal = !scope.environment || scope.environment === 'local'
  const setScheduler = async (on: boolean) => {
    setError(undefined)
    try {
      await backend.setSchedulerEnabled?.(on, idempotencyKey('sched'))
      envs.reload()
    } catch (x) {
      setError(asOgunError(x))
    }
  }
  const row = (x: EnvironmentView) => (
    <div class="row" key={x.id}>
      <span class="well">
        <Icon name={x.kind === 'edge' ? 'compass' : 'globe'} />
      </span>
      <div class="text">
        <div class="title">
          {x.name} <Chip>{x.kind}</Chip> {x.connected ? <Chip tone="ok">connected</Chip> : <Chip tone="bad">not reachable</Chip>}
        </div>
        <div class="desc">
          {[x.endpoint, x.version ? `v${x.version}` : '', x.detail, x.last_seen_at ? `seen ${fmtAgo(x.last_seen_at)}` : ''].filter(Boolean).join(' · ')}
        </div>
        {x.error ? <div class="desc err">{x.error}</div> : null}
      </div>
    </div>
  )
  return (
    <>
      <PageHead title="Environments" sub="The daemon this Studio talks to, the hubs and edges it can see, and the bridges that connect them.">
        <button type="button" class="btn" onClick={() => setAdding(true)}>
          <Icon name="plus" /> Add hub
        </button>
      </PageHead>
      <ScopeBar workflows={false} environments />
      {info?.engine && backend.engine ? <EngineGroup engine={info.engine} onChanged={() => (refresh(), envs.reload())} /> : null}
      <ErrorBox error={envs.error} />
      <ErrorBox error={error} title="Environment" />
      {envs.loading && !e && backend.environments ? <Loading /> : null}
      {!backend.environments ? (
        <>
          <Notice>This daemon does not list its environments over its API. What Studio knows is below.</Notice>
          <div class="panel">
            <Kv
              rows={[
                ['link', info?.link === 'local' ? `ogund on this machine${info.profile ? `, profile ${info.profile}` : ''}` : info?.link === 'hub' ? `hub ${info.hubUrl ?? ''}, read-only` : info?.link === 'server' ? 'ogun-server, signed in' : 'not connected'],
                ['state', info?.connected ? 'connected' : info?.error ? `${info.error.message} [${info.error.code}]` : 'not connected'],
                ['platform', info?.platform ?? ''],
              ]}
            />
            {info && !info.mobile && info.link !== 'local' && backend.linkLocal ? (
              <div class="toolbar" style={{ marginTop: '12px' }}>
                <button type="button" class="btn" onClick={() => void backend.linkLocal?.().then(refresh)}>
                  Use ogund on this machine
                </button>
              </div>
            ) : null}
            {info?.link === 'hub' && backend.linkForget ? (
              <div class="toolbar" style={{ marginTop: '12px' }}>
                <button type="button" class="btn" onClick={() => void backend.linkForget?.().then(refresh)}>
                  Forget the saved hub
                </button>
              </div>
            ) : null}
          </div>
        </>
      ) : null}
      {e && local && showLocal ? (
        <SettingsGroup title={info?.engine && backend.engine ? 'Scheduler and clients' : local.name}>
          <SettingRow title="Scheduler" desc="Fires cron triggers and starts runs for stream windows. Turn off to pause every schedule at once; runs already started finish.">
            <Switch label="Scheduler" checked={e.scheduler_enabled} disabled={!backend.setSchedulerEnabled} onChange={(on) => void setScheduler(on)} />
          </SettingRow>
          <SettingRow title="Version" desc={[local.endpoint, local.detail].filter(Boolean).join(' · ')}>
            <span class="muted">{local.version}</span>
          </SettingRow>
          {info?.engine && backend.engine ? null : <SettingRow title="Network access" desc="Limited to this machine. ogund never listens on the network: its one listener is a local socket. A hub (ogun-server) is a separate process in front of it." />}
          <div class="row">
            <div class="text">
              <div class="title">
                Authorized clients <span class="muted">{creds.data ? `${creds.data.credentials.filter((c) => !c.revoked).length} live` : ''}</span>
              </div>
              <div class="desc">Local credentials of kind human, agent, service and bridge. Tokens are in owner-only files and are never shown.</div>
            </div>
            <div class="control">
              <button type="button" class="btn small" onClick={() => setClients((v) => !v)} aria-expanded={clients}>
                {clients ? 'Hide' : 'Show'}
              </button>
            </div>
          </div>
          {clients ? (
            <div class="row stack">
              {creds.data ? (
                <div class="table-wrap flush">
                  <table>
                    <thead>
                      <tr>
                        <th>Name</th>
                        <th>Kind</th>
                        <th>Created</th>
                        <th>State</th>
                      </tr>
                    </thead>
                    <tbody>
                      {creds.data.credentials.map((c) => (
                        <tr>
                          <td class="mono">{c.name}</td>
                          <td class="mono">{c.kind}</td>
                          <td>{fmtTime(c.created_at)}</td>
                          <td>{c.revoked ? <span class="state s-cancelled">revoked</span> : 'live'}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <p class="muted">Credentials are listed only on a local link with a human credential.</p>
              )}
              <CodeCopy text="ogun credentials revoke <id>" />
            </div>
          ) : null}
        </SettingsGroup>
      ) : null}
      {e && showLocal ? (
        <SettingsGroup title="Bridges" note="Each bridge is its own process with its own credential, and none of them can decide an approval.">
          {e.bridges.map((b) => (
            <SettingRow title={<>{b.name} {bridgeChip(b)}</>} desc={b.detail} key={b.id} />
          ))}
        </SettingsGroup>
      ) : null}
      {e ? (
        <SettingsGroup title="Hubs and edges">
          {others.length === 0 ? (
            <div class="row">
              <div class="text">
                <div class="desc">No other environment{scope.environment ? ' matches the scope' : ''}.</div>
              </div>
            </div>
          ) : (
            others.map(row)
          )}
        </SettingsGroup>
      ) : null}
      {backend.hubs ? <HubsGroup onAdd={() => setAdding(true)} onChanged={() => (refresh(), envs.reload())} /> : null}
      {adding ? <AddHub onClose={() => setAdding(false)} onDone={() => (refresh(), envs.reload())} /> : null}
    </>
  )
}
