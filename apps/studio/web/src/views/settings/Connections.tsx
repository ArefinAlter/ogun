import { useEffect, useMemo, useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { ConnectionInput, ConnectionTestResult, ConnectionValue, ConnectionView, OgunError } from '../../api/types'
import type { ConnectorManifest } from '../../connectors'
import { CONNECTION_NAME, SECRET_NAME, categoryLabel, defaultSecretName, expandDomains, nodeSnippet, secretCommand, useCatalog } from '../../connectors'
import { fmtAgo, fmtTime, idempotencyKey } from '../../lib/format'
import { href, navigate, useAsync, useBackend } from '../../lib/hooks'
import { Chip, CodeCopy, Dialog, Empty, ErrorBox, Loading, Notice, PageHead, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { SecretEntry } from '../../ui/SecretEntry'

function StatePill({ c }: { c: ConnectionView }) {
  if (c.state === 'healthy') return <span class="state s-succeeded">healthy</span>
  if (c.state === 'failing') return <span class="state s-failed" title={c.last_error}>failing</span>
  return <span class="state s-cancelled">untested</span>
}

function NotSupported() {
  return (
    <>
      <PageHead title="Connections" sub="Named, configured uses of a connector." />
      <Notice>
        This daemon does not manage data connections yet (O-16 is a proposal). Until it does, a workflow names its settings and <span class="mono">secret_ref</span>s directly: open a connector under Connectors, and its "In a workflow" tab shows the node and the commands to run.
      </Notice>
      <a class="btn" href={href('settings', 'connectors')}>
        Browse connectors
      </a>
    </>
  )
}

export function TestPanel({ result }: { result: ConnectionTestResult }) {
  return (
    <div class={`testpanel ${result.ok ? 'ok' : 'bad'}`} role="status">
      <div class="testhead">
        <span class={`state ${result.ok ? 's-succeeded' : 's-failed'}`}>{result.ok ? 'passed' : 'failed'}</span>
        <span class="muted">{result.elapsed_ms} ms{result.simulated ? ' · simulated: nothing left this tab' : ''}</span>
      </div>
      <ul>
        {result.checks.map((c) => (
          <li class={c.ok ? 'ok' : 'bad'}>
            <span class="mark">{c.ok ? '✓' : '✗'}</span> <strong>{c.name}</strong> <span class="muted">{c.detail}</span>
          </li>
        ))}
      </ul>
    </div>
  )
}

export function ConnectionsList() {
  const backend = useBackend()
  const catalog = useCatalog()
  const list = useAsync(() => (backend.connections ? backend.connections.list() : Promise.resolve(null)), [backend])
  const [error, setError] = useState<OgunError | undefined>()
  const [testing, setTesting] = useState<string | null>(null)
  const [results, setResults] = useState<Record<string, ConnectionTestResult>>({})
  const [removing, setRemoving] = useState<ConnectionView | null>(null)
  if (!backend.connections) return <NotSupported />
  const byId = new Map((catalog ?? []).map((m) => [m.id, m]))
  const test = async (name: string) => {
    setTesting(name)
    setError(undefined)
    try {
      const r = await backend.connections?.test(name)
      if (r) setResults((x) => ({ ...x, [name]: r }))
      list.reload()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setTesting(null)
    }
  }
  const remove = async (c: ConnectionView) => {
    setRemoving(null)
    try {
      await backend.connections?.remove(c.name, idempotencyKey('rm'))
      list.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  const rows = list.data?.connections ?? []
  return (
    <>
      <PageHead title="Connections" sub="Named, configured uses of a connector: plain settings, and the names of the secrets that hold the rest.">
        <a class="btn primary" href={href('settings', 'connectors')}>
          New connection
        </a>
      </PageHead>
      <ErrorBox error={list.error} />
      <ErrorBox error={error} title="Connection" />
      {list.loading && !list.data ? <Loading /> : null}
      {list.data && rows.length === 0 ? <Empty>No connections yet. Pick a connector to create one.</Empty> : null}
      {rows.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Connector</th>
                <th>State</th>
                <th>Last test</th>
                <th>Used by</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((c) => {
                const m = byId.get(c.connector)
                return (
                  <>
                    <tr>
                      <td>
                        <a class="mono" href={href('settings', 'connections', c.name)}>
                          {c.name}
                        </a>
                      </td>
                      <td>
                        <a href={href('settings', 'connectors', c.connector)}>{m?.name ?? c.connector}</a>
                        {m ? <span class="muted"> · {categoryLabel(m.category)}</span> : null}
                      </td>
                      <td>
                        <StatePill c={c} />
                      </td>
                      <td title={fmtTime(c.last_test_at)}>{c.last_test_at ? `${fmtAgo(c.last_test_at)}${c.last_test_ms ? ` · ${c.last_test_ms} ms` : ''}` : <span class="muted">never</span>}</td>
                      <td class="wrap">{c.used_by.length ? c.used_by.map((w) => <a href={href('workflows', w)} style={{ marginRight: '8px' }}>{w}</a>) : <span class="muted">no workflow yet</span>}</td>
                      <td class="actions-cell">
                        <button type="button" class="btn small" disabled={testing === c.name} onClick={() => void test(c.name)}>
                          {testing === c.name ? 'Testing…' : 'Test'}
                        </button>
                        <button type="button" class="btn small ghost danger" onClick={() => setRemoving(c)} aria-label={`Delete ${c.name}`}>
                          Delete
                        </button>
                      </td>
                    </tr>
                    {results[c.name] ? (
                      <tr class="detail-row">
                        <td colSpan={6}>
                          <TestPanel result={results[c.name] as ConnectionTestResult} />
                        </td>
                      </tr>
                    ) : null}
                  </>
                )
              })}
            </tbody>
          </table>
        </div>
      ) : null}
      {removing ? (
        <Dialog title={`Delete ${removing.name}?`} onClose={() => setRemoving(null)}>
          <p>
            The connection is removed from Studio and the daemon. Secrets it names stay in the secret store, and workflows that spell out the same settings keep working.
            {removing.used_by.length ? ` ${removing.used_by.join(', ')} refers to it.` : ''}
          </p>
          <div class="decide-row">
            <button type="button" class="btn" onClick={() => setRemoving(null)}>
              Keep it
            </button>
            <button type="button" class="btn danger" onClick={() => void remove(removing)}>
              Delete connection
            </button>
          </div>
        </Dialog>
      ) : null}
    </>
  )
}

interface Form {
  name: string
  auth: string
  settings: Record<string, ConnectionValue>
  secretRefs: Record<string, string>
}

function initialForm(m: ConnectorManifest, existing?: ConnectionView): Form {
  const settings: Record<string, ConnectionValue> = {}
  for (const s of m.settings) if (s.default !== undefined) settings[s.name] = s.default
  if (existing)
    return { name: existing.name, auth: existing.auth, settings: { ...settings, ...existing.settings }, secretRefs: { ...existing.secret_refs } }
  return { name: '', auth: m.auth[0]?.id ?? '', settings, secretRefs: {} }
}

export function problems(m: ConnectorManifest, f: Form, taken: Set<string>, editing: boolean): string[] {
  const out: string[] = []
  if (!CONNECTION_NAME.test(f.name)) out.push('The name is lower case letters, digits, hyphens and underscores, starting with a letter (up to 64).')
  else if (!editing && taken.has(f.name)) out.push(`A connection named ${f.name} already exists.`)
  for (const s of m.settings) {
    const v = f.settings[s.name]
    if (s.required && (v === undefined || v === '')) out.push(`${s.label} is required.`)
    if (s.type === 'integer' && v !== undefined && v !== '' && !Number.isInteger(Number(v))) out.push(`${s.label} is a whole number.`)
  }
  const auth = m.auth.find((a) => a.id === f.auth)
  for (const field of auth?.fields ?? []) {
    if (field.secret) {
      const ref = f.secretRefs[field.name] ?? ''
      if (field.required && !ref) out.push(`${field.label}: name the secret that holds it.`)
      else if (ref && !SECRET_NAME.test(ref)) out.push(`${field.label}: a secret name has letters, digits, dots, hyphens and underscores.`)
    } else if (field.required && !String(f.settings[field.name] ?? '').trim()) out.push(`${field.label} is required.`)
  }
  return out
}

export function ConnectionEditor({ name, connectorId }: { name?: string; connectorId?: string }) {
  const backend = useBackend()
  const catalog = useCatalog()
  const existing = useAsync(() => (backend.connections && name && name !== 'new' ? backend.connections.list() : Promise.resolve(null)), [backend, name])
  const all = useAsync(() => (backend.connections ? backend.connections.list() : Promise.resolve(null)), [backend])
  const secrets = useAsync(() => backend.secretsList().catch(() => ({ names: [] as string[] })), [backend])
  const editing = !!name && name !== 'new'
  const current = editing ? existing.data?.connections.find((c) => c.name === name) : undefined
  const manifest = catalog?.find((m) => m.id === (current?.connector ?? connectorId))
  const [form, setForm] = useState<Form | null>(null)
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<ConnectionTestResult | null>(null)
  const [showProblems, setShowProblems] = useState(false)
  const [saved, setSaved] = useState(false)
  useEffect(() => {
    if (manifest && !form && (!editing || current)) setForm(initialForm(manifest, current))
  }, [manifest, current, editing, form])
  const taken = useMemo(() => new Set((all.data?.connections ?? []).map((c) => c.name)), [all.data])
  if (!backend.connections) return <NotSupported />
  if (!catalog || (editing && !existing.data)) return <Loading />
  if (!manifest || !form)
    return (
      <>
        <PageHead title="New connection" />
        <Empty>
          {editing ? `No connection is named ${name}.` : 'Choose a connector first.'} <a href={href('settings', 'connectors')}>Browse connectors</a>
        </Empty>
      </>
    )
  const auth = manifest.auth.find((a) => a.id === form.auth)
  const have = new Set(secrets.data?.names ?? [])
  const issues = problems(manifest, form, taken, editing || saved)
  const patch = (p: Partial<Form>) => setForm((f) => (f ? { ...f, ...p } : f))
  const setSetting = (key: string, v: ConnectionValue) => patch({ settings: { ...form.settings, [key]: v } })
  const input = (): ConnectionInput => {
    const settings: Record<string, ConnectionValue> = {}
    for (const s of manifest.settings) if (form.settings[s.name] !== undefined && form.settings[s.name] !== '') settings[s.name] = form.settings[s.name] as ConnectionValue
    for (const f of auth?.fields ?? []) if (!f.secret && form.settings[f.name] !== undefined && form.settings[f.name] !== '') settings[f.name] = form.settings[f.name] as ConnectionValue
    const secret_refs: Record<string, string> = {}
    for (const f of auth?.fields ?? []) if (f.secret && form.secretRefs[f.name]) secret_refs[f.name] = form.secretRefs[f.name] as string
    return { name: form.name, connector: manifest.id, auth: form.auth, settings, secret_refs }
  }
  const save = async (andTest: boolean) => {
    setShowProblems(true)
    if (issues.length) return
    setBusy(true)
    setError(undefined)
    try {
      await backend.connections?.put(input(), idempotencyKey('cnx'))
      setSaved(true)
      if (andTest) {
        const r = await backend.connections?.test(form.name)
        if (r) setResult(r)
        all.reload()
        if (r?.ok) setTimeout(() => navigate(href('settings', 'connections')), 1500)
      } else navigate(href('settings', 'connections'))
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const domains = expandDomains(manifest, { ...form.settings })
  const snippetOp = manifest.operators[0]
  return (
    <>
      <PageHead title={editing ? form.name : `New ${manifest.name} connection`} sub={`${manifest.vendor} · ${categoryLabel(manifest.category)}`}>
        <a class="btn" href={href('settings', 'connections')}>
          Cancel
        </a>
      </PageHead>
      <ErrorBox error={error} title="Save" />
      <SettingsGroup title="Name">
        <SettingRow title="Connection name" desc="How workflows and this list refer to it. It cannot be changed later.">
          <input aria-label="Connection name" value={form.name} disabled={editing || saved} placeholder={`${manifest.id}-main`} onInput={(e) => patch({ name: (e.target as HTMLInputElement).value.toLowerCase() })} />
        </SettingRow>
      </SettingsGroup>
      {manifest.settings.length ? (
        <SettingsGroup title="Settings" note="Plain values. They are stored with the connection and appear in a workflow's configuration.">
          {manifest.settings.map((s) => (
            <SettingRow title={<>{s.label}{s.required ? <span class="req"> required</span> : null}</>} desc={s.help}>
              {s.type === 'boolean' ? (
                <Switch label={s.label} checked={form.settings[s.name] === true} onChange={(v) => setSetting(s.name, v)} />
              ) : s.type === 'enum' ? (
                <select aria-label={s.label} value={String(form.settings[s.name] ?? '')} onChange={(e) => setSetting(s.name, (e.target as HTMLSelectElement).value)}>
                  {!s.required ? <option value="">not set</option> : null}
                  {s.options?.map((o) => (
                    <option value={o}>{o}</option>
                  ))}
                </select>
              ) : (
                <input aria-label={s.label} type={s.type === 'integer' ? 'number' : 'text'} value={String(form.settings[s.name] ?? '')} placeholder={s.placeholder ?? ''} size={30} onInput={(e) => setSetting(s.name, (e.target as HTMLInputElement).value)} />
              )}
            </SettingRow>
          ))}
        </SettingsGroup>
      ) : null}
      <SettingsGroup title="Authentication" note={backend.engine ? 'Choose how Ogun proves who it is. A connection holds only the name of each secret; enter the value once and the engine keeps it in the credential store of this computer.' : 'Choose how Ogun proves who it is. Studio takes no secret value: name the secret, then store it on a terminal.'}>
        {manifest.auth.length > 1 ? (
          <SettingRow title="Method">
            <select aria-label="Authentication method" value={form.auth} onChange={(e) => patch({ auth: (e.target as HTMLSelectElement).value })}>
              {manifest.auth.map((a) => (
                <option value={a.id}>{a.label}</option>
              ))}
            </select>
          </SettingRow>
        ) : null}
        {auth && auth.fields.length === 0 ? (
          <SettingRow title={auth.label} desc="Nothing to store. The daemon host's own identity is used." />
        ) : null}
        {auth?.fields.map((f) =>
          f.secret ? (
            <div class="row stack" key={f.name}>
              <div class="text">
                <div class="title">
                  {f.label}
                  {f.required ? <span class="req"> required</span> : null} <Chip tone="warn">secret</Chip>
                </div>
                {f.help ? <div class="desc">{f.help}</div> : null}
              </div>
              <div class="secretrow">
                <label class="field">
                  Secret name
                  <input
                    aria-label={`${f.label}: secret name`}
                    class="mono"
                    value={form.secretRefs[f.name] ?? ''}
                    placeholder={defaultSecretName(form.name, f.name)}
                    size={34}
                    onFocus={() => {
                      if (!form.secretRefs[f.name]) patch({ secretRefs: { ...form.secretRefs, [f.name]: defaultSecretName(form.name, f.name) } })
                    }}
                    onInput={(e) => patch({ secretRefs: { ...form.secretRefs, [f.name]: (e.target as HTMLInputElement).value } })}
                  />
                </label>
                {form.secretRefs[f.name] ? (
                  have.has(form.secretRefs[f.name] as string) ? (
                    <Chip tone="ok">stored</Chip>
                  ) : (
                    <Chip tone="bad">not stored yet</Chip>
                  )
                ) : null}
              </div>
              {backend.engine && form.secretRefs[f.name] && SECRET_NAME.test(form.secretRefs[f.name] as string) ? (
                <SecretEntry name={form.secretRefs[f.name] as string} stored={have.has(form.secretRefs[f.name] as string)} onChanged={() => secrets.reload()} />
              ) : form.secretRefs[f.name] && !have.has(form.secretRefs[f.name] as string) ? (
                <div class="stack-tight">
                  <CodeCopy text={secretCommand(form.secretRefs[f.name] as string)} label="Copy command" />
                  {backend.demo ? (
                    <button
                      type="button"
                      class="btn small"
                      onClick={() => {
                        backend.demo?.simulateSecret(form.secretRefs[f.name] as string)
                        secrets.reload()
                      }}
                    >
                      Check again
                    </button>
                  ) : null}
                </div>
              ) : null}
            </div>
          ) : (
            <SettingRow title={<>{f.label}{f.required ? <span class="req"> required</span> : null}</>} desc={f.help} key={f.name}>
              <input aria-label={f.label} value={String(form.settings[f.name] ?? '')} size={30} onInput={(e) => setSetting(f.name, (e.target as HTMLInputElement).value)} />
            </SettingRow>
          ),
        )}
      </SettingsGroup>
      {domains.length ? (
        <SettingsGroup title="Hosts this connection will contact" note="A workflow that uses it needs an egress approval for these. A host with braces is waiting for a setting above.">
          <div class="row stack">
            <ul class="mono namelist">
              {domains.map((d) => (
                <li class={d.complete ? '' : 'pending'}>{d.host}</li>
              ))}
            </ul>
          </div>
        </SettingsGroup>
      ) : null}
      {snippetOp ? (
        <SettingsGroup title="In a workflow">
          <div class="row stack">
            <CodeCopy text={nodeSnippet(manifest, snippetOp, { authId: form.auth, values: form.settings, secretRefs: form.secretRefs, connection: form.name })} label="Copy node" />
          </div>
        </SettingsGroup>
      ) : null}
      {showProblems && issues.length ? (
        <div class="notice warn" role="alert">
          <strong>Before you can save:</strong>
          <ul class="issues">
            {issues.map((i) => (
              <li>{i}</li>
            ))}
          </ul>
        </div>
      ) : null}
      {result ? <TestPanel result={result} /> : null}
      <div class="toolbar sticky-actions">
        <button type="button" class="btn primary" disabled={busy} onClick={() => void save(false)}>
          Save connection
        </button>
        <button type="button" class="btn" disabled={busy} onClick={() => void save(true)}>
          Save and test
        </button>
        {manifest.maturity === 'contract' ? <span class="muted">No operator exists for {manifest.name} yet, so a test cannot reach it.</span> : null}
      </div>
    </>
  )
}
