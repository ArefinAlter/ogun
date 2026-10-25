import { useMemo, useState } from 'preact/hooks'

import { asOgunError } from '../api'
import type { EventView, NodeState, OgunError, RunView } from '../api/types'
import { fmtAgo, fmtCount, fmtTime, idempotencyKey, words } from '../lib/format'
import { href, useAsync, useBackend, useInfo, useLive } from '../lib/hooks'
import { Dag } from '../ui/Dag'
import { Dialog, Empty, ErrorBox, Hash, Kv, Loading, PageHead, StateLabel, Tabs } from '../ui/components'

const PAGE = 50

export function Runs({ workflow }: { workflow?: string }) {
  const backend = useBackend()
  const [cursors, setCursors] = useState<string[]>([])
  const before = cursors[cursors.length - 1]
  const runs = useAsync(
    () => backend.runsList({ limit: PAGE, ...(workflow ? { workflow } : {}), ...(before ? { before } : {}) }),
    [backend, workflow, before],
  )
  // Live refresh only on the first page, where new runs appear.
  useLive(() => runs.reload(), undefined, cursors.length === 0)
  return (
    <>
      <PageHead title={workflow ? `Runs of ${workflow}` : 'Runs'}>
        {workflow ? (
          <a class="btn" href={href('runs')}>
            All runs
          </a>
        ) : null}
      </PageHead>
      <ErrorBox error={runs.error} />
      {runs.loading && !runs.data ? <Loading /> : null}
      {runs.data && runs.data.runs.length === 0 ? <Empty>No runs.</Empty> : null}
      {runs.data && runs.data.runs.length > 0 ? <RunTable runs={runs.data.runs} /> : null}
      <div class="pager">
        <button type="button" class="btn small" disabled={!cursors.length} onClick={() => setCursors((c) => c.slice(0, -1))}>
          Newer
        </button>
        <button
          type="button"
          class="btn small"
          disabled={!runs.data?.next}
          onClick={() => runs.data?.next && setCursors((c) => [...c, runs.data?.next as string])}
        >
          Older
        </button>
      </div>
    </>
  )
}

function RunTable({ runs }: { runs: RunView[] }) {
  return (
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Run</th>
            <th>Workflow</th>
            <th>State</th>
            <th class="num">Partitions</th>
            <th class="hide-narrow">Hash</th>
            <th>Created</th>
          </tr>
        </thead>
        <tbody>
          {runs.map((r) => (
            <tr>
              <td>
                <a class="mono" href={href('runs', r.run_id)}>
                  {r.run_id}
                </a>
              </td>
              <td>
                <a href={href('workflows', r.workflow, { version: r.version })}>{r.workflow}</a>{' '}
                <span class="muted">v{r.version}</span>
              </td>
              <td>
                <StateLabel state={r.state} />
              </td>
              <td class="num">{fmtCount(r.partition_count)}</td>
              <td class="hide-narrow">
                <Hash value={r.workflow_hash} short />
              </td>
              <td title={fmtTime(r.created_at)}>{fmtAgo(r.created_at)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

const TERMINAL = new Set(['succeeded', 'failed', 'cancelled'])
const MAX_EVENTS = 300

function describeKind(kind: unknown): string {
  if (typeof kind === 'string') return kind
  if (kind && typeof kind === 'object') {
    const o = kind as Record<string, unknown>
    const type = (o.type ?? o.kind ?? Object.keys(o)[0]) as string | undefined
    const parts: string[] = []
    if (type) parts.push(String(type))
    for (const k of ['from', 'to', 'state', 'cause', 'node', 'partition', 'attempt']) {
      if (o[k] !== undefined && typeof o[k] !== 'object') parts.push(`${k}=${String(o[k])}`)
    }
    return parts.join(' ')
  }
  return String(kind)
}

export function RunDetail({ runId }: { runId: string }) {
  const backend = useBackend()
  const { info } = useInfo()
  const run = useAsync(() => backend.runsGet(runId), [backend, runId])
  const r = run.data
  const wf = useAsync(
    () => (r ? backend.workflowsGet(r.workflow, r.version) : Promise.resolve(null)),
    [backend, r?.workflow, r?.version],
  )
  const [events, setEvents] = useState<EventView[]>([])
  const [tab, setTab] = useState<'nodes' | 'graph' | 'events'>('nodes')
  const [stateFilter, setStateFilter] = useState<string>('')
  const [nodeFilter, setNodeFilter] = useState<string>('')
  const [confirmCancel, setConfirmCancel] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()

  const liveError = useLive(
    (e) => {
      setEvents((prev) => [e, ...prev].slice(0, MAX_EVENTS))
      run.reload()
    },
    runId,
    !r || !TERMINAL.has(r.state),
  )

  const counts = useMemo(() => {
    const m = new Map<string, Map<NodeState, number>>()
    for (const n of r?.nodes ?? []) {
      const per = m.get(n.node) ?? new Map<NodeState, number>()
      per.set(n.state, (per.get(n.state) ?? 0) + 1)
      m.set(n.node, per)
    }
    return m
  }, [r])
  const totals = useMemo(() => {
    const m = new Map<string, number>()
    for (const n of r?.nodes ?? []) m.set(n.state, (m.get(n.state) ?? 0) + 1)
    return m
  }, [r])
  const nodes = (r?.nodes ?? []).filter((n) => (!stateFilter || n.state === stateFilter) && (!nodeFilter || n.node === nodeFilter))
  const shown = nodes.slice(0, 500)

  const cancel = async () => {
    setConfirmCancel(false)
    try {
      await backend.runsCancel(runId, idempotencyKey('cancel'))
      run.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }

  return (
    <>
      <PageHead title={<span class="mono" style={{ fontSize: '22px' }}>{runId}</span>}>
        <a class="btn" href={href('artifacts', { run: runId })}>
          Artifacts and lineage
        </a>
        {info?.canWrite && r && !TERMINAL.has(r.state) ? (
          <button type="button" class="btn danger" onClick={() => setConfirmCancel(true)}>
            Cancel run
          </button>
        ) : null}
      </PageHead>
      <ErrorBox error={run.error} />
      <ErrorBox error={error} title="Cancel" />
      {run.loading && !r ? <Loading /> : null}
      {r ? (
        <>
          <div class="panel">
            <Kv
              rows={[
                ['state', <StateLabel state={r.state} />],
                ['workflow', <a href={href('workflows', r.workflow, { version: r.version })}>{`${r.workflow} v${r.version}`}</a>],
                ['workflow hash', <Hash value={r.workflow_hash} />],
                ['partitions', fmtCount(r.partition_count)],
                ['created', `${fmtTime(r.created_at)} (${fmtAgo(r.created_at)})`],
                [
                  'node instances',
                  [...totals.entries()].map(([s, c]) => (
                    <span style={{ marginRight: '12px' }}>
                      <StateLabel state={s} /> {fmtCount(c)}
                    </span>
                  )),
                ],
                ['live updates', TERMINAL.has(r.state) ? 'run finished' : liveError ? `interrupted: ${liveError.message}` : 'on'],
              ]}
            />
          </div>
          <Tabs
            tabs={[
              ['nodes', `Node instances (${fmtCount(r.nodes?.length ?? 0)})`],
              ['graph', 'Graph'],
              ['events', `Events (${events.length})`],
            ]}
            value={tab}
            onChange={setTab}
          />
          {tab === 'nodes' ? (
            <>
              <div class="toolbar">
                <select aria-label="Filter by state" value={stateFilter} onChange={(e) => setStateFilter((e.target as HTMLSelectElement).value)}>
                  <option value="">every state</option>
                  {[...totals.keys()].map((s) => (
                    <option value={s}>{s}</option>
                  ))}
                </select>
                <select aria-label="Filter by node" value={nodeFilter} onChange={(e) => setNodeFilter((e.target as HTMLSelectElement).value)}>
                  <option value="">every node</option>
                  {[...counts.keys()].map((n) => (
                    <option value={n}>{n}</option>
                  ))}
                </select>
                {nodes.length > shown.length ? <span class="muted">showing {shown.length} of {fmtCount(nodes.length)}</span> : null}
              </div>
              {shown.length ? (
                <div class="table-wrap">
                  <table>
                    <thead>
                      <tr>
                        <th>Node</th>
                        <th>Partition</th>
                        <th>State</th>
                        <th class="num">Attempt</th>
                        <th>Cause</th>
                        <th>Updated</th>
                      </tr>
                    </thead>
                    <tbody>
                      {shown.map((n) => (
                        <tr>
                          <td class="mono">{n.node}</td>
                          <td class="mono">{n.partition || '–'}</td>
                          <td>
                            <StateLabel state={n.state} />
                          </td>
                          <td class="num">{n.attempt}</td>
                          <td class="mono">{n.last_cause ? words(n.last_cause) : '–'}</td>
                          <td title={fmtTime(n.updated_at)}>{fmtAgo(n.updated_at)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <Empty>No node instances{stateFilter || nodeFilter ? ' match' : ''}.</Empty>
              )}
            </>
          ) : null}
          {tab === 'graph' ? (
            wf.data ? (
              <Dag
                nodes={wf.data.workflow.nodes}
                states={counts}
                selected={nodeFilter || undefined}
                onSelect={(id) => {
                  setNodeFilter(id)
                  setTab('nodes')
                }}
              />
            ) : wf.error ? (
              <ErrorBox error={wf.error} />
            ) : (
              <Loading />
            )
          ) : null}
          {tab === 'events' ? (
            events.length ? (
              <div class="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th class="num">Seq</th>
                      <th>At</th>
                      <th>Instance</th>
                      <th>Event</th>
                    </tr>
                  </thead>
                  <tbody>
                    {events.map((e) => (
                      <tr>
                        <td class="num">{e.seq}</td>
                        <td>{fmtTime(e.at)}</td>
                        <td class="mono">{e.instance_id ?? '–'}</td>
                        <td class="mono wrap">{describeKind(e.kind)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <Empty>Events of this run appear here as they happen.</Empty>
            )
          ) : null}
        </>
      ) : null}
      {confirmCancel ? (
        <Dialog title="Cancel this run?" onClose={() => setConfirmCancel(false)}>
          <p>
            Every node instance of <span class="mono">{runId}</span> that has not finished is cancelled. Attempts in
            flight stop at their next checkpoint. Partitions already committed stay committed.
          </p>
          <div class="decide-row">
            <button type="button" class="btn" onClick={() => setConfirmCancel(false)}>
              Keep running
            </button>
            <button type="button" class="btn danger" onClick={cancel}>
              Cancel run
            </button>
          </div>
        </Dialog>
      ) : null}
    </>
  )
}

