import { useState } from 'preact/hooks'

import { asOgunError } from '../api'
import type { OgunError, PartitionRange, PlanBackfillResult } from '../api/types'
import { fmtCount, idempotencyKey } from '../lib/format'
import { href, navigate, useAsync, useBackend, useInfo, useLive } from '../lib/hooks'
import { Calendar, Legend, Strip } from '../ui/Strip'
import { Empty, ErrorBox, Hash, Loading, Notice, PageHead, StateLabel } from '../ui/components'

function RangeTable({ ranges }: { ranges: PartitionRange[] }) {
  return (
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>From</th>
            <th>To</th>
            <th class="num">Keys</th>
            <th>State</th>
          </tr>
        </thead>
        <tbody>
          {ranges.map((r) => (
            <tr>
              <td class="mono">{r.from}</td>
              <td class="mono">{r.to}</td>
              <td class="num">{fmtCount(r.count)}</td>
              <td>
                <StateLabel state={r.state} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

export function Partitions({ workflow }: { workflow?: string }) {
  const backend = useBackend()
  const list = useAsync(() => backend.workflowsList(), [backend])
  return (
    <>
      <PageHead title="Partitions" />
      <div class="form-row">
        <label class="field">
          Workflow
          <select
            value={workflow ?? ''}
            onChange={(e) => navigate(href('partitions', { workflow: (e.target as HTMLSelectElement).value }))}
          >
            <option value="">choose a workflow</option>
            {list.data?.workflows.map((w) => (
              <option value={w.name}>{w.name}</option>
            ))}
          </select>
        </label>
      </div>
      <ErrorBox error={list.error} />
      {workflow ? <WorkflowPartitions workflow={workflow} /> : <Empty>Choose a workflow to see its partitions.</Empty>}
    </>
  )
}

function WorkflowPartitions({ workflow }: { workflow: string }) {
  const backend = useBackend()
  const { info } = useInfo()
  const [from, setFrom] = useState('')
  const [to, setTo] = useState('')
  const [applied, setApplied] = useState<{ from?: string; to?: string }>({})
  const status = useAsync(
    () =>
      backend.partitionsStatus({
        workflow,
        ...(applied.from ? { from: applied.from } : {}),
        ...(applied.to ? { to: applied.to } : {}),
      }),
    [backend, workflow, applied],
  )
  useLive(() => status.reload())
  const [plan, setPlan] = useState<PlanBackfillResult | null>(null)
  const [planFrom, setPlanFrom] = useState('')
  const [planTo, setPlanTo] = useState('')
  const [force, setForce] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  const [key, setKey] = useState(() => idempotencyKey('backfill'))

  const preview = async () => {
    setError(undefined)
    setPlan(null)
    if (!planFrom || !planTo) return setError(asOgunError({ code: 'api.invalid_params', message: 'give both ends of the range' }))
    setBusy(true)
    try {
      setPlan(await backend.backfillsPlan(workflow, planFrom, planTo))
      setKey(idempotencyKey('backfill'))
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const start = async () => {
    if (!plan) return
    setBusy(true)
    setError(undefined)
    try {
      const run = await backend.runsStart({
        workflow,
        workflow_hash: plan.workflow_hash,
        partitions: { kind: 'range', from: planFrom, to: planTo, force },
        idempotency_key: key,
      })
      navigate(href('runs', run.run_id))
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const s = status.data
  const total = s ? s.ranges.reduce((a, r) => a + r.count, 0) : 0
  return (
    <>
      <div class="panel">
        <div class="form-row">
          <label class="field">
            From key
            <input value={from} placeholder="earliest" onInput={(e) => setFrom((e.target as HTMLInputElement).value)} />
          </label>
          <label class="field">
            To key
            <input value={to} placeholder="latest complete" onInput={(e) => setTo((e.target as HTMLInputElement).value)} />
          </label>
          <button type="button" class="btn" onClick={() => setApplied({ from, to })}>
            Show
          </button>
        </div>
        <ErrorBox error={status.error} />
        {status.loading && !s ? <Loading /> : null}
        {s ? (
          <>
            <p class="muted">
              {fmtCount(total)} keys in {s.ranges.length} ranges for workflow hash <Hash value={s.workflow_hash} short />.
            </p>
            <Strip ranges={s.ranges} />
            <Legend />
            <Calendar ranges={s.ranges} />
            <h3>Ranges</h3>
            {s.ranges.length ? <RangeTable ranges={s.ranges} /> : <Empty>No partitions in this range.</Empty>}
          </>
        ) : null}
      </div>
      <div class="panel">
        <h2>Backfill</h2>
        <p class="muted">
          A plan lists what a backfill over the range would run for the current workflow hash; keys already done are
          skipped unless you force them.
        </p>
        <div class="form-row">
          <label class="field">
            From key
            <input value={planFrom} onInput={(e) => setPlanFrom((e.target as HTMLInputElement).value)} />
          </label>
          <label class="field">
            To key
            <input value={planTo} onInput={(e) => setPlanTo((e.target as HTMLInputElement).value)} />
          </label>
          <button type="button" class="btn" disabled={busy} onClick={preview}>
            Preview plan
          </button>
        </div>
        <ErrorBox error={error} />
        {plan ? (
          <>
            <p>
              <strong>{fmtCount(plan.to_run)}</strong> keys to run, {fmtCount(plan.up_to_date)} already up to date, for
              workflow hash <Hash value={plan.workflow_hash} short />.
            </p>
            <Strip ranges={plan.ranges} />
            <Legend />
            <RangeTable ranges={plan.ranges} />
            {info?.canWrite ? (
              <div class="toolbar" style={{ marginTop: '12px' }}>
                <label class="check">
                  <input type="checkbox" checked={force} onChange={(e) => setForce((e.target as HTMLInputElement).checked)} />
                  also re-run keys already done
                </label>
                <button
                  type="button"
                  class="btn primary"
                  disabled={busy || (plan.to_run === 0 && !force)}
                  onClick={start}
                >
                  Start backfill
                </button>
              </div>
            ) : (
              <Notice>Starting a backfill needs the desktop Studio on the daemon host, or an editor role on Studio-web.</Notice>
            )}
          </>
        ) : null}
      </div>
    </>
  )
}
