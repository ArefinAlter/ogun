import { useMemo } from 'preact/hooks'

import type { RunView } from '../api/types'
import { fmtAgo, fmtCount, fmtTime } from '../lib/format'
import { href, useAsync, useBackend, useInfo, useLive } from '../lib/hooks'
import { ErrorBox, Kv, Loading, Notice, PageHead, StateLabel } from '../ui/components'
import { Welcome } from './Welcome'

const ACTIVE = new Set(['running', 'waiting', 'pending'])

/** One active run: its partitions as a meter, live. */
function ActiveRun({ run }: { run: RunView }) {
  const backend = useBackend()
  const detail = useAsync(() => backend.runsGet(run.run_id), [backend, run.run_id])
  useLive(() => detail.reload(), run.run_id)
  const r = detail.data ?? run
  const p = useMemo(() => {
    const byPartition = new Map<string, string[]>()
    for (const n of r.nodes ?? []) byPartition.set(n.partition, [...(byPartition.get(n.partition) ?? []), n.state])
    let done = 0
    let running = 0
    let failed = 0
    let waiting = 0
    for (const states of byPartition.values()) {
      if (states.every((s) => s === 'succeeded')) done++
      else if (states.some((s) => s === 'waiting_for_user')) waiting++
      else if (states.some((s) => s === 'running' || s === 'retry_scheduled' || s === 'ready')) running++
      else if (states.some((s) => s === 'failed')) failed++
    }
    const total = byPartition.size || r.partition_count
    return { done, running, failed, waiting, total }
  }, [r])
  const pct = p.total ? (p.done / p.total) * 100 : 0
  return (
    <a class="stat" href={href('runs', run.run_id)}>
      <div class="label">
        {r.workflow} <span class="muted">v{r.version}</span>
      </div>
      <div class="value" style={{ fontSize: '18px' }}>
        {fmtCount(p.done)} <span class="muted" style={{ fontWeight: 400 }}>of {fmtCount(p.total)} partitions done</span>
      </div>
      <div class="meter" style={{ margin: '8px 0 6px' }}>
        <span style={{ width: `${pct.toFixed(1)}%` }} />
      </div>
      <div class="hint">
        <StateLabel state={r.state} />{' '}
        {p.running ? `${fmtCount(p.running)} running` : ''}
        {p.waiting ? `${p.running ? ' · ' : ''}${fmtCount(p.waiting)} waiting for a person` : ''}
        {p.failed ? `${p.running || p.waiting ? ' · ' : ''}${fmtCount(p.failed)} failed` : ''}
        {!p.running && !p.waiting && !p.failed ? 'starting' : ''}
        {' · '}
        {fmtAgo(r.created_at)}
      </div>
    </a>
  )
}

export function Overview() {
  const backend = useBackend()
  const { info } = useInfo()
  const status = useAsync(() => backend.status(), [backend])
  const runs = useAsync(() => backend.runsList({ limit: 60 }), [backend])
  const liveError = useLive(() => {
    status.reload()
    runs.reload()
  })
  const s = status.data
  const all = runs.data?.runs ?? []
  const active = all.filter((r) => ACTIVE.has(r.state))
  // The newest run of each workflow: a stream that seals a window a minute would otherwise fill the list.
  const latest = useMemo(() => {
    const seen = new Set<string>()
    const out: RunView[] = []
    for (const r of all) {
      if (seen.has(r.workflow)) continue
      seen.add(r.workflow)
      out.push(r)
    }
    return out.slice(0, 10)
  }, [all])
  return (
    <>
      <PageHead title="Overview" sub={info?.link === 'local' && info.engine ? 'What your engine holds and what it is doing now. The page follows the event stream.' : 'What this daemon holds and what it is doing now. The page follows the event stream.'} />
      {s ? <Welcome workflows={s.workflows} /> : null}
      {info && !info.connected ? (
        <Notice kind="warn">
          {info.link === 'none'
            ? 'Not connected. Connect to a hub in Settings.'
            : info.link === 'local'
              ? info.engine && info.engine.state !== 'external'
                ? 'The engine is not answering. Restart it in Settings, Environments.'
                : 'ogund is not reachable on this machine. Start ogund, then reload.'
              : info.link === 'server' && info.error?.code === 'api.unauthenticated'
                ? 'Not signed in. Sign in to this server, then reload.'
                : `Cannot reach the ${info.link === 'hub' ? 'hub' : 'server'}${info.error ? `: ${info.error.message}` : '.'}`}
        </Notice>
      ) : null}
      <ErrorBox error={status.error} title="Daemon status" />
      {status.loading && !s ? <Loading /> : null}
      {s ? (
        <div class="grid">
          <a class="stat" href={href('workflows')}>
            <div class="label">Workflows</div>
            <div class="value">{fmtCount(s.workflows)}</div>
            <div class="hint">stored, each version by hash</div>
          </a>
          <a class="stat" href={href('runs')}>
            <div class="label">Runs active</div>
            <div class="value">{fmtCount(s.runs_active)}</div>
            <div class="hint">running or waiting for a person</div>
          </a>
          <a class="stat" href={href('approvals')}>
            <div class="label">Approvals pending</div>
            <div class="value">{fmtCount(s.approvals_pending)}</div>
            <div class="hint">decided only by a person</div>
          </a>
          <a class="stat" href={href('streams')}>
            <div class="label">Streams</div>
            <div class="value">{fmtCount(s.streams)}</div>
            <div class="hint">sensor sources sealing windows</div>
          </a>
        </div>
      ) : null}
      {active.length ? (
        <>
          <h2>Active runs</h2>
          <div class="grid two">
            {active.slice(0, 6).map((r) => (
              <ActiveRun run={r} key={r.run_id} />
            ))}
          </div>
        </>
      ) : null}
      <h2>Latest run of each workflow</h2>
      <ErrorBox error={runs.error} />
      {runs.data && latest.length === 0 ? <p class="muted">No runs yet.</p> : null}
      {latest.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Workflow</th>
                <th>Run</th>
                <th>State</th>
                <th class="num">Partitions</th>
                <th>Created</th>
              </tr>
            </thead>
            <tbody>
              {latest.map((r) => (
                <tr>
                  <td>
                    <a href={href('workflows', r.workflow, { version: r.version })}>{r.workflow}</a> <span class="muted">v{r.version}</span>
                  </td>
                  <td>
                    <a class="mono" href={href('runs', r.run_id)}>
                      {r.run_id}
                    </a>
                  </td>
                  <td>
                    <StateLabel state={r.state} />
                  </td>
                  <td class="num">{fmtCount(r.partition_count)}</td>
                  <td title={fmtTime(r.created_at)}>{fmtAgo(r.created_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      <p class="muted" style={{ fontSize: '13px' }}>
        <a href={href('runs')}>Every run</a>, newest first.
      </p>
      {s ? (
        <div class="panel">
          <h2>{info?.link === 'local' && info.engine ? 'Engine' : 'Daemon'}</h2>
          <Kv
            rows={[
              ['version', <span class="mono">{s.daemon_version}</span>],
              ['profile', <span class="mono">{s.profile ?? 'default'}</span>],
              ['started', `${fmtTime(s.started_at)} (${fmtAgo(s.started_at)})`],
              ['scheduler', s.scheduler_enabled ? 'firing triggers' : 'paused'],
              ['last event', <span class="mono">{fmtCount(s.last_event_seq)}</span>],
              [
                'connection',
                info?.link === 'local'
                  ? info.engine && info.engine.state !== 'external'
                    ? `Studio's own engine${info.engine.pid ? ` (process ${info.engine.pid})` : ''}`
                    : `local ogund${info.profile ? ` (profile ${info.profile})` : ''}`
                  : info?.link === 'hub'
                    ? info.hubUser ? `hub ${info.hubUrl ?? ''} (${info.hubUser}${info.hubRole ? `, ${info.hubRole}` : ''})` : `hub ${info.hubUrl ?? ''} (read-only)`
                    : info?.link === 'server'
                      ? `ogun-server${info.user ? `, signed in as ${info.user.name}` : ''}`
                      : 'none',
              ],
              ['live updates', liveError ? `interrupted: ${liveError.message}` : 'on'],
            ]}
          />
        </div>
      ) : null}
    </>
  )
}
