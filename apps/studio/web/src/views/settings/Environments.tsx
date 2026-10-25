import { useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { BridgeView, EnvironmentView, OgunError } from '../../api/types'
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

function AddHub({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const backend = useBackend()
  const [url, setUrl] = useState('')
  const [token, setToken] = useState('')
  const [ca, setCa] = useState('')
  const [remember, setRemember] = useState(true)
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
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
    <Dialog title="Connect to a hub" onClose={onClose}>
      {backend.linkHub ? (
        <>
          <p class="muted">A hub is a self-hosted ogun-server. Studio connects read-only with an API token, which acts as an agent and can never decide an approval. The token stays in Studio's native process; this page never reads it back.</p>
          <div class="form-row">
            <label class="field">
              Hub URL
              <input value={url} placeholder="https://hub.example.org" size={36} onInput={(e) => setUrl((e.target as HTMLInputElement).value)} />
            </label>
            <label class="field">
              API token
              <input type="password" autocomplete="off" value={token} size={30} onInput={(e) => setToken((e.target as HTMLInputElement).value)} />
            </label>
          </div>
          <label class="field">
            CA certificate (PEM), only for a hub with a private CA
            <textarea value={ca} style={{ minHeight: '90px' }} onInput={(e) => setCa((e.target as HTMLTextAreaElement).value)} />
          </label>
          <label class="check" style={{ margin: '8px 0' }}>
            <input type="checkbox" checked={remember} onChange={(e) => setRemember((e.target as HTMLInputElement).checked)} />
            remember this hub on this device
          </label>
          <ErrorBox error={error} />
          <div class="decide-row">
            <button type="button" class="btn" onClick={onClose}>
              Cancel
            </button>
            <button type="button" class="btn primary" disabled={busy || !url || !token} onClick={() => void connect()}>
              Connect
            </button>
          </div>
        </>
      ) : (
        <>
          <p>
            Only the desktop app links to a hub. {backend.demo ? 'This build is not linked to a hub.' : 'Studio-web is served by the hub itself, so it is already there.'} On the hub, a person with a browser session creates the API token; it acts as an agent.
          </p>
          <div class="decide-row">
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
          <Icon name="plus" /> Add environment
        </button>
      </PageHead>
      <ScopeBar workflows={false} environments />
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
        <SettingsGroup title={local.name}>
          <SettingRow title="Scheduler" desc="Fires cron triggers and starts runs for stream windows. Turn off to pause every schedule at once; runs already started finish.">
            <Switch label="Scheduler" checked={e.scheduler_enabled} disabled={!backend.setSchedulerEnabled} onChange={(on) => void setScheduler(on)} />
          </SettingRow>
          <SettingRow title="Version" desc={[local.endpoint, local.detail].filter(Boolean).join(' · ')}>
            <span class="muted">{local.version}</span>
          </SettingRow>
          <SettingRow title="Network access" desc="Limited to this machine. ogund never listens on the network: its one listener is a local socket. A hub (ogun-server) is a separate process in front of it." />
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
      {adding ? <AddHub onClose={() => setAdding(false)} onDone={() => (refresh(), envs.reload())} /> : null}
    </>
  )
}
