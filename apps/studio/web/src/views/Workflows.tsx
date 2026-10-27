import { useMemo, useState } from 'preact/hooks'

import { asOgunError } from '../api'
import { svcInfo } from '../api/svc'
import type { OgunError, PartitionSelector, PutWorkflowResult, ValidateResult, WorkflowDoc } from '../api/types'
import { fmtAgo, fmtTime, idempotencyKey } from '../lib/format'
import { href, navigate, useAsync, useBackend, useInfo } from '../lib/hooks'
import { Dag } from '../ui/Dag'
import { ImportBanner, SampleImport } from './Welcome'
import { Empty, ErrorBox, Hash, JsonBlock, Kv, Loading, Notice, PageHead, Tabs, Untrusted } from '../ui/components'

export function Workflows() {
  const backend = useBackend()
  const { info } = useInfo()
  const list = useAsync(() => backend.workflowsList(), [backend])
  const [importing, setImporting] = useState(false)
  return (
    <>
      <PageHead title="Workflows">
        {info?.canWrite ? (
          <button type="button" class="btn" onClick={() => setImporting((v) => !v)}>
            {importing ? 'Close import' : 'Import a workflow'}
          </button>
        ) : null}
      </PageHead>
      {importing ? <ImportWorkflow onStored={() => list.reload()} /> : null}
      <ImportBanner />
      <ErrorBox error={list.error} />
      {list.loading && !list.data ? <Loading /> : null}
      {list.data && list.data.workflows.length === 0 ? (
        <>
          <Empty>No workflows stored. Store one with `ogun` or import it here.</Empty>
          {backend.engine ? (
            <div class="panel">
              <h2>Start from a sample project</h2>
              <SampleImport compact onImported={() => list.reload()} />
            </div>
          ) : null}
        </>
      ) : null}
      {list.data && list.data.workflows.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th class="num">Version</th>
                <th>Hash</th>
                <th>Triggers</th>
                <th>Runnable</th>
                <th class="hide-narrow">Updated</th>
              </tr>
            </thead>
            <tbody>
              {list.data.workflows.map((w) => (
                <tr>
                  <td>
                    <a href={href('workflows', w.name)}>{w.name}</a>
                  </td>
                  <td class="num">{w.latest_version}</td>
                  <td>
                    <Hash value={w.workflow_hash} short />
                  </td>
                  <td>{w.enabled ? 'enabled' : 'paused'}</td>
                  <td>{w.runnable ? 'yes' : <span class="state s-waiting_for_user">needs approval</span>}</td>
                  <td class="hide-narrow">{fmtAgo(w.updated_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </>
  )
}

function parseDoc(text: string): { doc?: unknown; error?: OgunError } {
  try {
    return { doc: JSON.parse(text) }
  } catch (e) {
    return { error: asOgunError({ code: 'api.invalid_params', message: `not JSON: ${e instanceof Error ? e.message : ''}` }) }
  }
}

function Diagnostics({ result }: { result: ValidateResult }) {
  return (
    <>
      <p>
        {result.ok ? 'Valid.' : 'Invalid.'}{' '}
        {result.workflow_hash ? (
          <>
            Workflow hash <Hash value={result.workflow_hash} />
          </>
        ) : null}
      </p>
      {result.diagnostics.length ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Severity</th>
                <th>Code</th>
                <th>Path</th>
                <th>Message</th>
              </tr>
            </thead>
            <tbody>
              {result.diagnostics.map((d) => (
                <tr>
                  <td>
                    <span class={`state ${d.severity === 'error' ? 's-failed' : 's-waiting_for_user'}`}>{d.severity}</span>
                  </td>
                  <td class="mono">{d.code}</td>
                  <td class="mono">{d.path || '/'}</td>
                  <td class="wrap">{d.message}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p class="muted">No diagnostics.</p>
      )}
    </>
  )
}

function ImportWorkflow({ onStored }: { onStored: () => void }) {
  const backend = useBackend()
  const [text, setText] = useState('')
  const [checked, setChecked] = useState<ValidateResult | null>(null)
  const [stored, setStored] = useState<PutWorkflowResult | null>(null)
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  const [key, setKey] = useState(() => idempotencyKey('put'))

  const validate = async () => {
    setError(undefined)
    setStored(null)
    const { doc, error: perr } = parseDoc(text)
    if (perr) return setError(perr)
    setBusy(true)
    try {
      setChecked(await backend.workflowsValidate(doc))
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const store = async () => {
    const { doc, error: perr } = parseDoc(text)
    if (perr) return setError(perr)
    setBusy(true)
    try {
      const r = await backend.workflowsPut(doc, key)
      setStored(r)
      setKey(idempotencyKey('put'))
      onStored()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <div class="panel">
      <h2>Import a workflow</h2>
      <p class="muted">
        Paste a workflow document (O-02 JSON). The daemon validates it; storing a version that references new code,
        new egress or a budget over policy creates approvals, which a person decides in the approvals inbox.
      </p>
      <textarea
        aria-label="Workflow document"
        value={text}
        spellcheck={false}
        onInput={(e) => {
          setText((e.target as HTMLTextAreaElement).value)
          setChecked(null)
        }}
      />
      <div class="toolbar" style={{ marginTop: '8px' }}>
        <button type="button" class="btn" disabled={busy || !text.trim()} onClick={validate}>
          Validate
        </button>
        <button type="button" class="btn primary" disabled={busy || !checked?.ok} onClick={store}>
          Store this version
        </button>
      </div>
      <ErrorBox error={error} />
      {checked ? <Diagnostics result={checked} /> : null}
      {stored ? (
        <Notice>
          Stored <a href={href('workflows', stored.name, { version: stored.version })}>{stored.name} v{stored.version}</a>{' '}
          {stored.created ? '' : '(already stored)'} · <Hash value={stored.workflow_hash} short />
          {stored.approvals.length ? (
            <div>
              Needs {stored.approvals.length} approval{stored.approvals.length > 1 ? 's' : ''} before it can run:{' '}
              {stored.approvals.map((a) => (
                <a href={href('approvals', a.approval_id)} style={{ marginRight: '8px' }}>
                  {a.kind}
                </a>
              ))}
            </div>
          ) : null}
        </Notice>
      ) : null}
    </div>
  )
}

export function WorkflowDetail({ name, version }: { name: string; version?: number }) {
  const backend = useBackend()
  const { info } = useInfo()
  const wf = useAsync(() => backend.workflowsGet(name, version), [backend, name, version])
  const sandboxless = useAsync(() => svcInfo().then((i) => !!i && !i.sandbox), []).data === true
  const [tab, setTab] = useState<'graph' | 'document' | 'diagnostics'>('graph')
  const [selected, setSelected] = useState<string | undefined>()
  const diag = useAsync(
    () => (tab === 'diagnostics' && wf.data ? backend.workflowsValidate(wf.data.workflow) : Promise.resolve(null)),
    [backend, tab, wf.data],
  )
  const d = wf.data
  const doc = d?.workflow as WorkflowDoc | undefined
  const node = useMemo(() => doc?.nodes.find((n) => n.id === selected), [doc, selected])
  return (
    <>
      <PageHead title={name}>
        <a class="btn" href={href('runs', { workflow: name })}>
          Runs
        </a>
        <a class="btn" href={href('partitions', { workflow: name })}>
          Partitions
        </a>
      </PageHead>
      <ErrorBox error={wf.error} />
      {wf.loading && !d ? <Loading /> : null}
      {doc && sandboxless && doc.nodes.some((n) => n.code) ? (
        <Notice kind="warn">
          This workflow has code nodes ({doc.nodes.filter((n) => n.code).map((n) => n.id).join(', ')}). This hub has no operator
          sandbox, so they cannot run here, and their code approval stays pending. Run it on your own daemon instead.
        </Notice>
      ) : null}
      {d && doc ? (
        <>
          <div class="panel">
            <div class="form-row">
              <label class="field">
                Version
                <select
                  value={String(d.version)}
                  onChange={(e) =>
                    navigate(href('workflows', name, { version: (e.target as HTMLSelectElement).value }))
                  }
                >
                  {Array.from({ length: d.summary.latest_version }, (_, i) => d.summary.latest_version - i).map((v) => (
                    <option value={String(v)}>
                      v{v}
                      {v === d.summary.latest_version ? ' (latest)' : ''}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <Kv
              rows={[
                ['workflow hash', <Hash value={d.workflow_hash} />],
                ['triggers', d.summary.enabled ? 'enabled' : 'paused'],
                ['runnable', d.summary.runnable ? 'every approval granted' : 'waiting for approvals'],
                ['stored', fmtTime(d.summary.updated_at)],
                ['nodes', String(doc.nodes.length)],
              ]}
            />
            <Untrusted label="Description, as written by the author" text={d.summary.untrusted_description ?? doc.description} />
          </div>
          {info?.canWrite ? <StartRun name={name} version={d.version} hash={d.workflow_hash} partitioned={!!doc.partitions} /> : null}
          <Tabs
            tabs={[
              ['graph', 'Graph'],
              ['document', 'Document (normal form)'],
              ['diagnostics', 'Diagnostics'],
            ]}
            value={tab}
            onChange={setTab}
          />
          {tab === 'graph' ? (
            <>
              <Dag nodes={doc.nodes} selected={selected} onSelect={setSelected} />
              {node ? (
                <div class="panel" style={{ marginTop: '12px' }}>
                  <h3>{node.id}</h3>
                  <Kv
                    rows={[
                      ['operator', <span class="mono">{node.op}</span>],
                      ['inputs', <span class="mono">{(node.inputs ?? []).join(', ') || '–'}</span>],
                      ['routes', <span class="mono">{(node.route ?? []).map((r) => r.to).join(', ') || '–'}</span>],
                      ['code', node.code ? <Hash value={node.code.blake3} /> : '–'],
                    ]}
                  />
                  <h3>Configuration and policy</h3>
                  <JsonBlock value={{ config: node.config ?? {}, policy: node.policy ?? {} }} />
                </div>
              ) : (
                <p class="muted">Select a node to see its configuration.</p>
              )}
            </>
          ) : null}
          {tab === 'document' ? <JsonBlock value={doc} /> : null}
          {tab === 'diagnostics' ? (
            <>
              <ErrorBox error={diag.error} />
              {diag.loading ? <Loading what="validating" /> : diag.data ? <Diagnostics result={diag.data} /> : null}
            </>
          ) : null}
        </>
      ) : null}
    </>
  )
}

function StartRun({ name, version, hash, partitioned }: { name: string; version: number; hash: string; partitioned: boolean }) {
  const backend = useBackend()
  const [kind, setKind] = useState<'latest' | 'keys' | 'range'>('latest')
  const [keys, setKeys] = useState('')
  const [from, setFrom] = useState('')
  const [to, setTo] = useState('')
  const [force, setForce] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  const [key, setKey] = useState(() => idempotencyKey('run'))
  const start = async () => {
    let partitions: PartitionSelector
    if (kind === 'keys') {
      const ks = keys.split(/[\s,]+/).filter(Boolean)
      if (!ks.length) return setError(asOgunError({ code: 'api.invalid_params', message: 'name at least one partition key' }))
      partitions = { kind: 'keys', keys: ks }
    } else if (kind === 'range') {
      if (!from || !to) return setError(asOgunError({ code: 'api.invalid_params', message: 'give both ends of the range' }))
      partitions = { kind: 'range', from, to, force }
    } else partitions = { kind: 'latest' }
    setBusy(true)
    setError(undefined)
    try {
      const run = await backend.runsStart({ workflow: name, version, workflow_hash: hash, partitions, idempotency_key: key })
      setKey(idempotencyKey('run'))
      navigate(href('runs', run.run_id))
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <details class="panel">
      <summary>Start a run of v{version}</summary>
      <p class="muted" style={{ marginTop: '8px' }}>
        The run is bound to workflow hash <Hash value={hash} short />; the daemon refuses it if the version changed.
      </p>
      <div class="form-row">
        <label class="field">
          Partitions
          <select value={kind} onChange={(e) => setKind((e.target as HTMLSelectElement).value as typeof kind)}>
            <option value="latest">{partitioned ? 'latest complete partition' : 'unpartitioned run'}</option>
            {partitioned ? <option value="keys">these keys</option> : null}
            {partitioned ? <option value="range">a range (backfill)</option> : null}
          </select>
        </label>
        {kind === 'keys' ? (
          <label class="field">
            Keys
            <input value={keys} placeholder="2026-10-01, 2026-10-02" onInput={(e) => setKeys((e.target as HTMLInputElement).value)} />
          </label>
        ) : null}
        {kind === 'range' ? (
          <>
            <label class="field">
              From
              <input value={from} onInput={(e) => setFrom((e.target as HTMLInputElement).value)} />
            </label>
            <label class="field">
              To
              <input value={to} onInput={(e) => setTo((e.target as HTMLInputElement).value)} />
            </label>
            <label class="check">
              <input type="checkbox" checked={force} onChange={(e) => setForce((e.target as HTMLInputElement).checked)} />
              re-run partitions already done
            </label>
          </>
        ) : null}
        <button type="button" class="btn primary" disabled={busy} onClick={start}>
          Start run
        </button>
      </div>
      <ErrorBox error={error} />
    </details>
  )
}
