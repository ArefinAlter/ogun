import { ErrorBox, Kv, Loading, Notice, PageHead, StateLabel } from '../ui/components'
import { fmtAgo, fmtCount, fmtTime } from '../lib/format'
import { href, useAsync, useBackend, useInfo, useLive } from '../lib/hooks'

export function Overview() {
  const backend = useBackend()
  const { info } = useInfo()
  const status = useAsync(() => backend.status(), [backend])
  const runs = useAsync(() => backend.runsList({ limit: 8 }), [backend])
  const liveError = useLive(() => {
    status.reload()
    runs.reload()
  })
  const s = status.data
  return (
    <>
      <PageHead title="Overview" />
      {info && !info.connected ? (
        <Notice kind="warn">
          {info.link === 'none'
            ? 'Not connected. Connect to a hub in Settings.'
            : info.link === 'local'
              ? 'ogund is not reachable on this machine. Start ogund, then reload.'
              : 'The server is not reachable with this session.'}
        </Notice>
      ) : null}
      <ErrorBox error={status.error} title="Daemon status" />
      {status.loading && !s ? <Loading /> : null}
      {s ? (
        <>
          <div class="grid">
            <a class="stat" href={href('workflows')}>
              <div class="label">Workflows</div>
              <div class="value">{fmtCount(s.workflows)}</div>
            </a>
            <a class="stat" href={href('runs')}>
              <div class="label">Runs active</div>
              <div class="value">{fmtCount(s.runs_active)}</div>
            </a>
            <a class="stat" href={href('approvals')}>
              <div class="label">Approvals pending</div>
              <div class="value">{fmtCount(s.approvals_pending)}</div>
            </a>
            <a class="stat" href={href('streams')}>
              <div class="label">Streams</div>
              <div class="value">{fmtCount(s.streams)}</div>
            </a>
          </div>
          <div class="panel" style={{ marginTop: '16px' }}>
            <h2>Daemon</h2>
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
                    ? `local ogund${info.profile ? ` (profile ${info.profile})` : ''}`
                    : info?.link === 'hub'
                      ? `hub ${info.hubUrl ?? ''} (read-only)`
                      : info?.link === 'server'
                        ? `ogun-server${info.user ? `, signed in as ${info.user.name}` : ''}`
                        : 'none',
                ],
                ['live updates', liveError ? `interrupted: ${liveError.message}` : 'on'],
              ]}
            />
          </div>
        </>
      ) : null}
      <h2>Recent runs</h2>
      <ErrorBox error={runs.error} />
      {runs.data && runs.data.runs.length === 0 ? <p class="muted">No runs yet.</p> : null}
      {runs.data && runs.data.runs.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Run</th>
                <th>Workflow</th>
                <th>State</th>
                <th class="num">Partitions</th>
                <th>Created</th>
              </tr>
            </thead>
            <tbody>
              {runs.data.runs.map((r) => (
                <tr>
                  <td>
                    <a class="mono" href={href('runs', r.run_id)}>
                      {r.run_id}
                    </a>
                  </td>
                  <td>
                    {r.workflow} <span class="muted">v{r.version}</span>
                  </td>
                  <td>
                    <StateLabel state={r.state} />
                  </td>
                  <td class="num">{fmtCount(r.partition_count)}</td>
                  <td>{fmtAgo(r.created_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </>
  )
}
