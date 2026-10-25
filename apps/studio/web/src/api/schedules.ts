// Trigger schedules read from the daemon itself: every stored workflow's latest version, its triggers and its
// latest run. Adding a schedule stores a new version with one more cron trigger, through `workflows.put`, so it
// is the daemon (and its approval rules) that decides what the new version needs.

import type { Backend } from './backend'
import { asOgunError } from './backend'
import { parseCron } from '../lib/cron'
import type { RunState, ScheduleView, WorkflowDoc } from './types'

export function realSchedules(b: Backend): Pick<Backend, 'schedules' | 'addSchedule'> {
  return {
    async schedules() {
      const list = await b.workflowsList()
      const runs = await b.runsList({ limit: 200 }).catch(() => ({ runs: [] }))
      const out: ScheduleView[] = []
      for (const w of list.workflows) {
        const got = await b.workflowsGet(w.name).catch(() => null)
        if (!got) continue
        const last = runs.runs.find((r) => r.workflow === w.name)
        for (const t of (got.workflow.triggers ?? []) as { kind?: string; cron?: string; source?: string }[]) {
          out.push({
            workflow: w.name,
            version: got.version,
            trigger_kind: String(t.kind ?? 'unknown'),
            ...(t.cron ? { cron: t.cron } : {}),
            ...(t.source ? { source: t.source } : {}),
            enabled: w.enabled,
            runnable: w.runnable,
            ...(last ? { last_run_at: last.created_at, last_run_state: last.state as RunState } : {}),
          })
        }
      }
      return { schedules: out }
    },
    async addSchedule(workflow, cron, key) {
      if (!parseCron(cron)) throw asOgunError({ code: 'api.invalid_params', class: 'invalid_argument', message: 'that is not a five-field cron expression (minute hour day-of-month month day-of-week)' })
      const got = await b.workflowsGet(workflow)
      const doc = JSON.parse(JSON.stringify(got.workflow)) as WorkflowDoc
      const triggers = (doc.triggers ?? []) as { kind?: string; cron?: string }[]
      if (triggers.some((t) => t.kind === 'cron' && t.cron === cron)) throw asOgunError({ code: 'schedule.exists', class: 'failed_precondition', message: `${workflow} already runs on ${cron}` })
      doc.triggers = [...triggers, { kind: 'cron', cron }] as WorkflowDoc['triggers']
      doc.version = got.version + 1
      const r = await b.workflowsPut(doc, key)
      return { workflow, version: r.version, approvals: r.approvals.length }
    },
  }
}
