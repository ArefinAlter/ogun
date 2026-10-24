import { useState } from 'preact/hooks'

import { asOgunError } from '../api'
import type { OgunError } from '../api/types'
import { fmtTime } from '../lib/format'
import { useAsync, useBackend, useInfo } from '../lib/hooks'
import { Empty, ErrorBox, JsonBlock, Kv, Loading, Notice, PageHead } from '../ui/components'

const SECRETISH = /(key|token|secret|password|authorization|cookie|credential)/i

/** Defence in depth: the daemon never returns provider keys, and Studio would not show one if it did. */
function withoutSecrets(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(withoutSecrets)
  if (v && typeof v === 'object') {
    const out: Record<string, unknown> = {}
    for (const [k, val] of Object.entries(v)) {
      out[k] = SECRETISH.test(k) && typeof val === 'string' ? '[not shown]' : withoutSecrets(val)
    }
    return out
  }
  return v
}

export function Settings() {
  const { info } = useInfo()
  return (
    <>
      <PageHead title="Settings" />
      <Connection />
      {info?.canReadSettings ? <LocalSettings /> : null}
      {info && !info.canReadSettings ? (
        <Notice>
          Local credentials and secret names are listed only on a local desktop link with a human credential.
        </Notice>
      ) : null}
      <Ladder />
    </>
  )
}

function Connection() {
  const backend = useBackend()
  const { info, refresh } = useInfo()
  const [url, setUrl] = useState('')
  const [token, setToken] = useState('')
  const [ca, setCa] = useState('')
  const [remember, setRemember] = useState(true)
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  const act = async (f: () => Promise<unknown>) => {
    setBusy(true)
    setError(undefined)
    try {
      await f()
      setToken('')
      refresh()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  if (!info) return null
  if (info.backend === 'http') {
    return (
      <div class="panel">
        <h2>Session</h2>
        <Kv
          rows={[
            ['server', window.location.origin],
            ['signed in as', info.user?.name ?? 'not signed in'],
            ['roles', info.user?.roles.join(', ') || '–'],
            ['approver', info.user?.approver ? 'yes (passkey)' : 'no'],
          ]}
        />
      </div>
    )
  }
  return (
    <div class="panel">
      <h2>Connection</h2>
      <Kv
        rows={[
          [
            'link',
            info.link === 'local'
              ? `ogund on this machine${info.profile ? `, profile ${info.profile}` : ''}`
              : info.link === 'hub'
                ? `hub ${info.hubUrl ?? ''}, read-only`
                : 'not connected',
          ],
          ['state', info.connected ? 'connected' : info.error ? `${info.error.message} [${info.error.code}]` : 'not connected'],
          ['platform', info.platform],
        ]}
      />
      {!info.mobile && info.link !== 'local' ? (
        <div class="toolbar" style={{ marginTop: '12px' }}>
          <button type="button" class="btn" disabled={busy} onClick={() => act(() => backend.linkLocal?.() ?? Promise.resolve())}>
            Use ogund on this machine
          </button>
        </div>
      ) : null}
      <h3>Connect to a hub</h3>
      <p class="muted">
        A hub is a self-hosted ogun-server. Studio connects read-only with an API token, which acts as an agent and
        can never decide an approval. The token stays in Studio's native process and its data directory; this page
        never reads it back.
      </p>
      <form
        onSubmit={(e) => {
          e.preventDefault()
          void act(() => backend.linkHub?.(url, token, ca.trim() ? ca : null, remember) ?? Promise.resolve())
        }}
      >
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
        <label class="field" style={{ marginBottom: '12px' }}>
          CA certificate (PEM), only for a hub with a private CA
          <textarea value={ca} style={{ minHeight: '90px' }} onInput={(e) => setCa((e.target as HTMLTextAreaElement).value)} />
        </label>
        <div class="toolbar">
          <label class="check">
            <input type="checkbox" checked={remember} onChange={(e) => setRemember((e.target as HTMLInputElement).checked)} />
            remember this hub on this device
          </label>
          <button type="submit" class="btn primary" disabled={busy || !url || !token}>
            Connect
          </button>
          {info.link === 'hub' ? (
            <button type="button" class="btn" disabled={busy} onClick={() => act(() => backend.linkForget?.() ?? Promise.resolve())}>
              Forget the saved hub
            </button>
          ) : null}
        </div>
      </form>
      <ErrorBox error={error} />
    </div>
  )
}

function LocalSettings() {
  const backend = useBackend()
  const creds = useAsync(() => backend.credentialsList(), [backend])
  const secrets = useAsync(() => backend.secretsList(), [backend])
  return (
    <>
      <div class="panel">
        <h2>Credentials</h2>
        <p class="muted">
          Local credentials of this daemon. Tokens are never shown; they live in owner-only files. Adding and revoking
          credentials happens in the ogun CLI on a terminal.
        </p>
        <ErrorBox error={creds.error} />
        {creds.loading && !creds.data ? <Loading /> : null}
        {creds.data && creds.data.credentials.length === 0 ? <Empty>No credentials.</Empty> : null}
        {creds.data && creds.data.credentials.length > 0 ? (
          <div class="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Kind</th>
                  <th>Id</th>
                  <th>Created</th>
                  <th>State</th>
                </tr>
              </thead>
              <tbody>
                {creds.data.credentials.map((c) => (
                  <tr>
                    <td class="mono">{c.name}</td>
                    <td class="mono">{c.kind}</td>
                    <td class="mono">{c.credential_id}</td>
                    <td>{fmtTime(c.created_at)}</td>
                    <td>{c.revoked ? <span class="state s-cancelled">revoked</span> : 'live'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : null}
      </div>
      <div class="panel">
        <h2>Secret names</h2>
        <p class="muted">
          Names a workflow can use in <code>{'{"secret_ref": "<name>"}'}</code>. Values are never shown and never leave
          the daemon; they are set with the ogun CLI on a terminal.
        </p>
        <ErrorBox error={secrets.error} />
        {secrets.data && secrets.data.names.length === 0 ? <Empty>No secrets.</Empty> : null}
        {secrets.data && secrets.data.names.length > 0 ? (
          <ul class="mono">
            {secrets.data.names.map((n) => (
              <li>{n}</li>
            ))}
          </ul>
        ) : null}
      </div>
    </>
  )
}

function Ladder() {
  const backend = useBackend()
  const ladder = useAsync(() => backend.llmLadder(), [backend])
  return (
    <div class="panel">
      <h2>LLM provider ladder</h2>
      <p class="muted">
        Read-only. Models are optional: a workflow without <code>llm.*</code> operators runs with no model access.
      </p>
      <ErrorBox error={ladder.error} />
      {ladder.data && !ladder.data.available ? (
        <p class="muted">This daemon does not report its ladder over this connection.</p>
      ) : null}
      {ladder.data?.available ? <JsonBlock value={withoutSecrets(ladder.data.ladder)} /> : null}
    </div>
  )
}
