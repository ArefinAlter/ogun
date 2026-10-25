import { useMemo, useState } from 'preact/hooks'

import type { ScheduleView, WorkflowDoc } from '../../api/types'
import { asOgunError } from '../../api'
import { describeCron, nextFire, parseCron } from '../../lib/cron'
import { fmtAgo, fmtTime, idempotencyKey } from '../../lib/format'
import { href, useAsync, useBackend, useInfo, useLive } from '../../lib/hooks'
import { useScope } from '../../lib/scope'
import { Chip, Dialog, Empty, ErrorBox, Loading, Notice, PageHead, StateLabel, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { ScopeBar } from '../../ui/ScopeBar'

/** The schedules a daemon reports, or, when it does not, derived from the stored workflows' triggers. */
function useSchedules() {
  const backend = useBackend()
  return useAsync(async () => {
    if (backend.schedules) return (await backend.schedules()).schedules
    const list = await backend.workflowsList()
    const out: ScheduleView[] = []
    for (const w of list.workflows) {
      const doc = (await backend.workflowsGet(w.name)).workflow as WorkflowDoc
      for (const t of (doc.triggers ?? []) as { kind?: string; cron?: string; source?: string }[]) {
        out.push({
          workflow: w.name,
          version: w.latest_version,
          trigger_kind: String(t.kind ?? 'unknown'),
          ...(t.cron ? { cron: t.cron } : {}),
          ...(t.source ? { source: t.source } : {}),
          enabled: w.enabled,
          runnable: w.runnable,
        })
      }
    }
    return out
  }, [backend])
}

function When({ s, now }: { s: ScheduleView; now: Date }) {
  if (s.trigger_kind === 'stream') return <span class="muted">continuous: each sealed window starts a run</span>
  if (!s.cron) return <span class="muted">–</span>
  if (!s.enabled) return <span class="muted">paused</span>
  const next = nextFire(s.cron, now)
  if (!next) return <span class="muted">never</span>
  return (
    <span title={fmtTime(next.toISOString())}>
      {fmtTime(next.toISOString())} <span class="muted">({fmtAgo(next.toISOString(), now.getTime())})</span>
    </span>
  )
}


const PRESETS: [string, string][] = [
  ['Every 15 minutes', '*/15 * * * *'],
  ['Every hour', '0 * * * *'],
  ['Every day at 02:00', '0 2 * * *'],
  ['Every Monday at 06:00', '0 6 * * 1'],
  ['On the 1st of each month at 03:00', '0 3 1 * *'],
]

function NewSchedule({ workflows, initial, onClose, onDone }: { workflows: string[]; initial: string; onClose: () => void; onDone: () => void }) {
  const backend = useBackend()
  const [workflow, setWorkflow] = useState(initial || workflows[0] || '')
  const [cron, setCron] = useState('0 2 * * *')
  const [error, setError] = useState<ReturnType<typeof asOgunError> | undefined>()
  const [result, setResult] = useState<{ version: number; approvals: number } | null>(null)
  const valid = !!parseCron(cron)
  const fires = useMemo(() => {
    const out: Date[] = []
    let t = new Date()
    for (let i = 0; i < 5 && valid; i++) {
      const n = nextFire(cron, t)
      if (!n) break
      out.push(n)
      t = n
    }
    return out
  }, [cron, valid])
  const create = async () => {
    setError(undefined)
    try {
      const r = await backend.addSchedule?.(workflow, cron.trim().replace(/\s+/g, ' '), idempotencyKey('sched'))
      if (r) setResult(r)
      onDone()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  return (
    <Dialog title="New schedule" onClose={onClose}>
      {result ? (
        <>
          <p>
            Stored <strong>{workflow} v{result.version}</strong> with the trigger <span class="mono">{cron}</span>.{' '}
            {result.approvals > 0 ? `It needs ${result.approvals} approval${result.approvals > 1 ? 's' : ''} before it can run on this schedule.` : 'It needs no approval: the trigger changes when a run starts, not what it may do.'}
          </p>
          <div class="decide-row">
            <a class="btn" href={href('workflows', workflow)} onClick={onClose}>
              Open the workflow
            </a>
            <button type="button" class="btn primary" onClick={onClose}>
              Done
            </button>
          </div>
        </>
      ) : (
        <>
          <p class="muted">A schedule is a trigger in the workflow document, so adding one stores a new version of the workflow with its own hash. Times are UTC.</p>
          <div class="form-row">
            <label class="field">
              Workflow
              <select value={workflow} onChange={(e) => setWorkflow((e.target as HTMLSelectElement).value)}>
                {workflows.map((w) => (
                  <option value={w}>{w}</option>
                ))}
              </select>
            </label>
            <label class="field">
              Cron (minute hour day-of-month month day-of-week)
              <input class="mono" value={cron} size={22} aria-invalid={!valid} onInput={(e) => setCron((e.target as HTMLInputElement).value)} />
            </label>
          </div>
          <div class="chips tight" style={{ marginBottom: '10px' }}>
            {PRESETS.map(([label, c]) => (
              <button type="button" class="catchip" onClick={() => setCron(c)}>
                {label}
              </button>
            ))}
          </div>
          {valid ? (
            <div class="notice">
              <strong>{describeCron(cron)}.</strong> Next: {fires.map((f) => fmtTime(f.toISOString())).join(' · ')}
            </div>
          ) : (
            <div class="notice error" role="alert">
              That is not a five-field cron expression. Use numbers, ranges (1-5), lists (1,15) and steps (*/10).
            </div>
          )}
          <ErrorBox error={error} />
          <div class="decide-row">
            <button type="button" class="btn" onClick={onClose}>
              Cancel
            </button>
            <button type="button" class="btn primary" disabled={!valid || !workflow} onClick={() => void create()}>
              Store with this trigger
            </button>
          </div>
        </>
      )}
    </Dialog>
  )
}

export function SchedulesSettings() {
  const backend = useBackend()
  const { info } = useInfo()
  const sched = useSchedules()
  useLive(() => sched.reload())
  const [error, setError] = useState<ReturnType<typeof asOgunError> | undefined>()
  const now = useMemo(() => new Date(), [sched.data])
  const scope = useScope()
  const [adding, setAdding] = useState(false)
  const workflowNames = [...new Set((sched.data ?? []).map((x) => x.workflow))]
  const rows = (sched.data ?? []).filter((x) => !scope.workflow || x.workflow === scope.workflow)
  const elsewhere = !!scope.environment && scope.environment !== 'local'
  const toggle = async (s: ScheduleView, enabled: boolean) => {
    setError(undefined)
    try {
      await backend.setWorkflowEnabled?.(s.workflow, enabled, idempotencyKey('enable'))
      sched.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  // On a server, pausing a workflow needs the admin role (O-13); elsewhere the connection's write access decides.
  const can = !!backend.setWorkflowEnabled && !!info?.canWrite && (info.link !== 'server' || !!info.user?.roles.some((r) => r === 'admin' || r === 'owner'))
  return (
    <>
      <PageHead title="Schedules" sub="What starts runs on its own: cron triggers and continuous streams. Times are in UTC unless you choose otherwise under General.">
        {backend.addSchedule ? (
          <button type="button" class="btn" onClick={() => setAdding(true)}>
            <Icon name="plus" /> New schedule
          </button>
        ) : null}
      </PageHead>
      <ScopeBar />
      <ErrorBox error={sched.error} />
      <ErrorBox error={error} title="Schedule" />
      {sched.loading && !sched.data ? <Loading /> : null}
      {elsewhere ? (
        <Empty>Schedules run on the daemon that holds the workflow. This environment has none of its own.</Empty>
      ) : null}
      {!elsewhere && sched.data && rows.length === 0 ? <Empty>No workflow{scope.workflow ? ` named ${scope.workflow}` : ''} has a trigger.</Empty> : null}
      {!elsewhere && sched.data && rows.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Workflow</th>
                <th>Trigger</th>
                <th>Next run</th>
                <th>Last run</th>
                <th>{can ? 'Enabled' : 'State'}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((s) => (
                <tr>
                  <td>
                    <a href={href('workflows', s.workflow)}>{s.workflow}</a> <span class="muted">v{s.version}</span>
                  </td>
                  <td>
                    {s.cron ? (
                      <span title={s.cron}>
                        {describeCron(s.cron)} <span class="mono muted">{s.cron}</span>
                      </span>
                    ) : (
                      <span>{s.trigger_kind}{s.source ? <span class="mono muted"> {s.source}</span> : null}</span>
                    )}
                  </td>
                  <td>
                    <When s={s} now={now} />
                  </td>
                  <td>{s.last_run_state ? <><StateLabel state={s.last_run_state} /> <span class="muted">{fmtAgo(s.last_run_at)}</span></> : <span class="muted">never</span>}</td>
                  <td>
                    {can ? (
                      <Switch label={`${s.workflow} enabled`} checked={s.enabled} onChange={(v) => void toggle(s, v)} />
                    ) : !s.runnable ? (
                      <span class="state s-waiting_for_user">needs approval</span>
                    ) : (
                      <span class={`state ${s.enabled ? 's-succeeded' : 's-cancelled'}`}>{s.enabled ? 'enabled' : 'paused'}</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      {adding ? <NewSchedule workflows={workflowNames} initial={scope.workflow} onClose={() => setAdding(false)} onDone={() => sched.reload()} /> : null}
      <Notice>
        A workflow version that references new code, new egress or a budget over policy does not run on its schedule until a person approves it. Pausing a schedule stops new runs; runs already started finish.
      </Notice>
    </>
  )
}
