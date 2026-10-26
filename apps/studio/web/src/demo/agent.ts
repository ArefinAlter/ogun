// The scripted agent (O-17, proposed). It answers a handful of data-engineering requests the way a real agent would
// be allowed to: through the closed tool table in `lib/agentTools.ts`, one tool call at a time, against the same
// `Backend` the pages use. Every number it reports is read from the daemon behind it when it is asked; only the choice
// of what to say is a script. No model is called, and the model picker says so.
//
// What it shows is the boundary, not the intelligence: access levels gate tools (a read-only thread cannot store a
// workflow, an Author thread can, an Operate thread can also start a run), anything with consequences is a card whose
// button a person presses, approvals are only ever listed, and a tool the settings switch off is refused.

import type { AgentApi, Backend, EventSubscription } from '../api/backend'
import { asOgunError } from '../api/backend'
import type {
  AgentAccess,
  AgentAction,
  AgentApprovalRef,
  AgentCard,
  AgentItem,
  AgentModel,
  AgentSettings,
  AgentThread,
  AgentThreadSummary,
  ApprovalView,
  Mention,
  PartitionRange,
  RunView,
  WorkflowDoc,
} from '../api/types'
import { categoryLabel, loadCatalog, matchesFilter, MATURITY } from '../connectors'
import { ACCESS_INFO, DEFAULT_INSTRUCTIONS, TOOLS, mayCall } from '../lib/agentTools'
import { parseCsv } from '../lib/csv'
import { asText, decodeBase64, idempotencyKey } from '../lib/format'
import { getPrefs } from '../lib/prefs'
import { clone, iso, sleep, ulid } from './util'

class Stopped extends Error {}

interface Live {
  thread: AgentThread
  stop: boolean
  running: boolean
}

interface Ctx {
  readonly access: AgentAccess
  readonly backend: Backend
  readonly mentions: Mention[]
  readonly names: string[]
  say(text: string): Promise<void>
  card(card: AgentCard): string
  patch(id: string, f: (card: AgentCard) => void): void
  notice(text: string, tone?: 'info' | 'warn'): void
  tool<T>(name: string, input: unknown, run: () => Promise<T> | T, summarize: (out: T) => string): Promise<T | undefined>
  pause(ms: number): Promise<void>
}

type Scenario = (c: Ctx, text: string) => Promise<void>

const HELP = [
  'I work on this daemon through a fixed set of tools: I can read workflows, runs, partitions, artifacts, lineage and approvals, validate a workflow document, and plan a backfill. With Author access I can also store a workflow version, and with Operate access start a run.',
  '',
  'I cannot decide an approval, read a secret value or run a shell command, and nothing you write in the instructions changes that.',
  '',
  'Try one of these:',
  '- `What is running, and what is waiting for a person?`',
  '- `Backfill trips-backfill since 2019`',
  '- `Why did the ridership forecast fail last night?`',
  '- `Draft a daily workflow that lists trips per station from the open-trips archive`',
  '- `Which stations are likely to run empty or full today?`',
  '- `Which connectors would I need to send alerts to Datadog and DingTalk?`',
].join('\n')

const CAUSES: Record<string, string> = {
  'http.timeout': 'The source did not answer within the node\'s timeout on every attempt. That is usually the provider being slow or down, not a fault in the workflow.',
  'http.throttled': 'The source asked Ogun to slow down. The node retries with a growing delay and the partition completes when the limit lifts.',
  daemon_restart: 'The daemon restarted while this attempt was running. The attempt runs again; finished partitions are untouched.',
}

const FILLER = /\b(a|an|the|to|for|and|or|of|in|on|with|my|our|me|i|we|you|please|need|want|would|which|what|that|this|send|sending|use|using|from|into)\b/g

function count(ranges: PartitionRange[], state: PartitionRange['state']): number {
  return ranges.filter((r) => r.state === state).reduce((n, r) => n + r.count, 0)
}

/** A thread's title: the first request, cut at a word. */
export function titleOf(text: string): string {
  const t = text.replace(/\s+/g, ' ').trim()
  if (!t) return 'New thread'
  if (t.length <= 56) return t
  const cut = t.slice(0, 56)
  const at = cut.lastIndexOf(' ')
  return (at > 30 ? cut.slice(0, at) : cut).replace(/[.,;:!?]+$/, '') + '…'
}

function monthNow(): string {
  return new Date().toISOString().slice(0, 7)
}

/** Keeps a tool's output small enough to show and store: the interface expands it on demand. */
function bounded(v: unknown, max = 3000): unknown {
  const s = JSON.stringify(v)
  if (s === undefined || s.length <= max) return v
  return { truncated: true, bytes: s.length, head: s.slice(0, max) }
}

// ---------------------------------------------------------------- scenarios

const status: Scenario = async (c) => {
  const st = await c.tool('status.get', {}, () => c.backend.status(), (s) => `${s.workflows} workflows, ${s.runs_active} runs active, ${s.approvals_pending} approvals pending`)
  const runs = await c.tool('runs.list', { limit: 10 }, () => c.backend.runsList({ limit: 10 }), (r) => `${r.runs.length} recent runs`)
  const approvals = await c.tool('approvals.list', { state: 'pending' }, () => c.backend.approvalsList('pending'), (a) => `${a.approvals.length} pending`)
  const active = (runs?.runs ?? []).filter((r) => r.state === 'running' || r.state === 'waiting')
  const lines: string[] = []
  for (const r of active) {
    const p = await c.tool('partitions.status', { workflow: r.workflow }, () => c.backend.partitionsStatus({ workflow: r.workflow }), (s) => `${count(s.ranges, 'done')} of ${s.ranges.reduce((n, x) => n + x.count, 0)} partitions done`)
    const done = p ? count(p.ranges, 'done') : 0
    const total = p ? p.ranges.reduce((n, x) => n + x.count, 0) : 0
    lines.push(
      r.state === 'running'
        ? `- **${r.workflow}** is running${total > 1 ? `: ${done} of ${total} partitions done` : ''}. [Open the run](#/runs/${r.run_id}).`
        : `- **${r.workflow}** is waiting for a person. [Open the run](#/runs/${r.run_id}).`,
    )
  }
  const failed = (runs?.runs ?? []).filter((r) => r.state === 'failed')
  await c.say(
    [
      `The daemon holds **${st?.workflows ?? '?'} workflows**. ${active.length ? 'Right now:' : 'Nothing is running right now.'}`,
      '',
      ...lines,
      failed.length ? `- **${failed[0]?.workflow}** failed in its latest run. Ask me why and I will read it.` : '',
      '',
      `${approvals?.approvals.length ?? 0} approval request${approvals?.approvals.length === 1 ? ' is' : 's are'} waiting for a person.`,
    ]
      .filter((l, i, a) => l !== '' || a[i - 1] !== '')
      .join('\n'),
  )
  if (approvals?.approvals.length) {
    c.card({ type: 'approvals', approvals: approvals.approvals.map(refOf) })
    await c.say('I can read these but I cannot decide them. Approvals are decided by a person on a terminal or in Studio, bound to the request hash, so a conversation is never where a decision is made.')
  }
}

function refOf(a: ApprovalView): AgentApprovalRef {
  return { approval_id: a.approval_id, kind: a.kind, workflow: a.workflow, ...(a.node ? { node: a.node } : {}), state: a.state, requested_by: a.requested_by }
}

const approvalsScenario: Scenario = async (c) => {
  const list = await c.tool('approvals.list', { state: 'pending' }, () => c.backend.approvalsList('pending'), (a) => `${a.approvals.length} pending`)
  const pending = list?.approvals ?? []
  if (!pending.length) {
    await c.say('No approval request is waiting right now.')
    return
  }
  await c.say(`**${pending.length} request${pending.length === 1 ? ' is' : 's are'} waiting for a person.** What each one asks for, from the facts the daemon recorded:`)
  const lines: string[] = []
  for (const a of pending) {
    const full = await c.tool('approvals.list', { approval_id: a.approval_id }, () => c.backend.approvalsGet(a.approval_id), () => `${a.kind.replace('_', ' ')} for ${a.workflow}`)
    const facts = (full?.facts ?? []).slice(0, 3).map((f) => `${f.label}: \`${f.value.length > 24 ? f.value.slice(0, 12) + '…' : f.value}\``)
    lines.push(`- **${a.kind.replace('_', ' ')}** for ${a.workflow}${a.node ? ` (node \`${a.node}\`)` : ''}. ${facts.join(', ')}.`)
  }
  await c.say(lines.join('\n'))
  c.card({ type: 'approvals', approvals: pending.map(refOf) })
  await c.say('The requester\'s own words are on each approval page, marked as not verified by Ogun. I have left them out on purpose: read them there, next to the facts.')
}

const failure: Scenario = async (c, text) => {
  const wf = workflowIn(c, text) ?? 'ridership-forecast'
  const runs = await c.tool('runs.list', { workflow: wf, limit: 5 }, () => c.backend.runsList({ workflow: wf, limit: 5 }), (r) => `${r.runs.length} runs of ${wf}`)
  const bad = (runs?.runs ?? []).find((r) => r.state === 'failed')
  if (!bad) {
    await c.say(`The latest runs of **${wf}** did not fail. Nothing to explain.`)
    return
  }
  const run = await c.tool('runs.get', { run_id: bad.run_id }, () => c.backend.runsGet(bad.run_id), (r) => `${r.nodes?.filter((n) => n.state === 'failed').length ?? 0} failed node instance`)
  const failed = run?.nodes?.find((n) => n.state === 'failed')
  const wfDoc = await c.tool('workflows.get', { name: wf }, () => c.backend.workflowsGet(wf), (g) => `${g.workflow.nodes.length} nodes, hash ${g.workflow_hash.slice(0, 12)}`)
  if (!failed) {
    await c.say(`Run \`${bad.run_id}\` is marked failed but I found no failed node instance in it.`)
    return
  }
  const node = wfDoc?.workflow.nodes.find((n) => n.id === failed.node)
  const skipped = (run?.nodes ?? []).filter((n) => n.state === 'skipped').map((n) => n.node)
  const cause = failed.last_cause ?? 'unknown'
  await c.say(
    [
      `**\`${failed.node}\` failed** in run [${bad.run_id}](#/runs/${bad.run_id}) for partition \`${failed.partition}\`, after ${failed.attempt} attempt${failed.attempt === 1 ? '' : 's'}. The recorded cause is \`${cause}\`.`,
      '',
      CAUSES[cause] ?? 'The daemon recorded no more detail than that.',
      '',
      node ? `The node runs \`${node.op}\`${node.config && 'allowed_domains' in node.config ? ` against ${(node.config.allowed_domains as string[]).map((d) => `\`${d}\``).join(', ')}` : ''}.` : '',
      skipped.length ? `${skipped.map((s) => `\`${s}\``).join(' and ')} did not run, so nothing built on the missing forecast was published.` : '',
      '',
      '**What I did not check:** whether the source is up now. I have no tool that reaches a host, which is the point of the egress approval.',
    ]
      .filter((l, i, a) => l !== '' || a[i - 1] !== '')
      .join('\n'),
  )
  c.card({ type: 'retry', run_id: bad.run_id, workflow: wf, node: failed.node, cause })
  await c.say(
    c.access === 'operate'
      ? 'I have Operate access in this thread, but a retry of a failed node is your call: press the button when the source is back.'
      : 'Retrying the partition is one press. If the timeout keeps recurring, the fix is a longer timeout in the node, which is a new workflow version: tell me and I will draft it.',
  )
}

const backfill: Scenario = async (c, text) => {
  const wf = workflowIn(c, text) ?? 'trips-backfill'
  const plan = c.card({
    type: 'plan',
    title: `Backfill ${wf}`,
    steps: [
      { text: 'Read the workflow and its partitioning', state: 'doing' },
      { text: 'See which partitions are done', state: 'todo' },
      { text: 'Plan the range', state: 'todo' },
      { text: 'Start it, or leave the decision to you', state: 'todo' },
    ],
  })
  const step = (i: number, state: 'doing' | 'done') =>
    c.patch(plan, (card) => {
      if (card.type === 'plan' && card.steps[i]) card.steps[i].state = state
    })
  const got = await c.tool('workflows.get', { name: wf }, () => c.backend.workflowsGet(wf), (g) => `${wf} v${g.version}, ${g.workflow.nodes.length} nodes`)
  const part = got?.workflow.partitions as { kind: string; granularity: string; start: string } | undefined
  if (!got || !part || part.kind !== 'time') {
    await c.say(`**${wf}** has no time partitions, so there is nothing to backfill: it runs as a whole.`)
    return
  }
  step(0, 'done')
  step(1, 'doing')
  const status = await c.tool('partitions.status', { workflow: wf }, () => c.backend.partitionsStatus({ workflow: wf }), (s) => `${count(s.ranges, 'done')} done, ${count(s.ranges, 'running')} running, ${count(s.ranges, 'failed')} failed`)
  step(1, 'done')
  step(2, 'doing')
  const from = part.start
  const to = part.granularity === 'month' ? monthNow() : new Date().toISOString().slice(0, 10)
  const plan2 = await c.tool('backfills.plan', { workflow: wf, from, to }, () => c.backend.backfillsPlan(wf, from, to), (p) => `${p.to_run} to run, ${p.up_to_date} up to date`)
  step(2, 'done')
  if (!status || !plan2) return
  await c.say(
    [
      `**${wf}** is partitioned by ${part.granularity} from \`${part.start}\`. ${count(status.ranges, 'done')} partitions are done${count(status.ranges, 'running') ? `, ${count(status.ranges, 'running')} are running` : ''}${count(status.ranges, 'failed') ? `, ${count(status.ranges, 'failed')} failed` : ''}.`,
      '',
      plan2.to_run === 0
        ? 'A backfill over the whole range would find nothing to do: every partition that can run has run, and a partition that is done never runs again.'
        : `A backfill from \`${from}\` to \`${to}\` would run **${plan2.to_run}** partition${plan2.to_run === 1 ? '' : 's'} and skip ${plan2.up_to_date} that are already done.`,
    ].join('\n'),
  )
  const id = c.card({ type: 'backfill', workflow: wf, from, to, to_run: plan2.to_run, up_to_date: plan2.up_to_date })
  if (plan2.to_run === 0) {
    step(3, 'done')
    return
  }
  if (c.access === 'operate') {
    step(3, 'doing')
    const started = await c.tool(
      'runs.start',
      { workflow: wf, partitions: { kind: 'range', from, to } },
      () => c.backend.runsStart({ workflow: wf, partitions: { kind: 'range', from, to }, idempotency_key: idempotencyKey('agent-backfill') }),
      (r) => `run ${r.run_id.slice(0, 18)}…`,
    )
    step(3, 'done')
    if (started) {
      c.patch(id, (card) => {
        if (card.type === 'backfill') card.started = { run_id: started.run_id }
      })
      c.card({ type: 'run', run_id: started.run_id, workflow: wf })
      await c.say('I started it because this thread has Operate access. The start is idempotent: asking again returns the same run. Any egress or budget approval it needs will wait for a person.')
    }
  } else {
    await c.say(`I have ${ACCESS_INFO[c.access].label} access in this thread, so I am leaving the start to you: the button above runs exactly this plan. Switch the thread to Operate if you want me to start runs myself.`)
  }
}

const SLUG_STOP = /\b(a|an|the|that|which|and|from|to|for|of|in|per|new|workflow|pipeline|job|daily|hourly|weekly|monthly|draft|create|write|build|make|lists?|downloads?|fetch(es)?)\b/g

function slugOf(text: string): string {
  const lower = text.toLowerCase()
  const base = lower
    .replace(SLUG_STOP, ' ')
    .replace(/[^a-z0-9]+/g, ' ')
    .trim()
    .split(' ')
    .filter(Boolean)
    .slice(0, 2)
    .join('-')
  const cadence = /\b(daily|hourly|weekly|monthly)\b/.exec(lower)?.[1]
  const slug = cadence && base ? `${base}-${cadence}` : base
  return /^[a-z]/.test(slug) && slug.length >= 3 ? slug.slice(0, 40) : 'trips-station-daily'
}

const draft: Scenario = async (c, text) => {
  const plan = c.card({
    type: 'plan',
    title: 'Draft a workflow',
    steps: [
      { text: 'Look at the stored workflows for something to reuse', state: 'doing' },
      { text: 'Write the document from built-in operators only', state: 'todo' },
      { text: 'Validate it', state: 'todo' },
      { text: 'Store it, or hand it to you', state: 'todo' },
    ],
  })
  const step = (i: number, state: 'doing' | 'done') =>
    c.patch(plan, (card) => {
      if (card.type === 'plan' && card.steps[i]) card.steps[i].state = state
    })
  const list = await c.tool('workflows.list', {}, () => c.backend.workflowsList(), (l) => `${l.workflows.length} stored workflows`)
  step(0, 'done')
  const similar = (list?.workflows ?? []).find((w) => /trips/.test(w.name))
  await c.say(
    similar
      ? `**${similar.name}** already reads the open-trips archive, month by month. I will not change it: a new workflow of its own keeps its history clean.`
      : 'Nothing stored does this yet, so I will write a new workflow.',
  )
  step(1, 'doing')
  await c.pause(500)
  let name = slugOf(text)
  if ((list?.workflows ?? []).some((w) => w.name === name)) name = `${name}-2`
  const doc: WorkflowDoc = {
    ir_version: 0,
    workflow: name,
    version: 1,
    description: 'The trip files of every station for yesterday, listed from the public open-trips archive, digested and put in a manifest. Built-in operators only, so no code needs approval.',
    triggers: [{ kind: 'cron', cron: '30 1 * * *' }],
    partitions: { kind: 'time', granularity: 'day', start: new Date().toISOString().slice(0, 8) + '01' },
    nodes: [
      { id: 'fetch', op: 'acquire@1', config: { allowed_domains: ['open-trips-archive.s3.amazonaws.com'], listing: 's3_prefix' } },
      { id: 'digest', op: 'hash@1', inputs: ['fetch.artifacts'] },
      { id: 'manifest', op: 'manifest@1', inputs: ['digest.artifacts'] },
    ],
  } as WorkflowDoc
  step(1, 'done')
  step(2, 'doing')
  const check = await c.tool('workflows.validate', { workflow: name }, () => c.backend.workflowsValidate(doc), (v) => (v.ok ? `valid, hash ${(v.workflow_hash ?? '').slice(0, 12)}` : `${v.diagnostics.filter((d) => d.severity === 'error').length} errors`))
  step(2, 'done')
  if (!check) return
  const needs = ['Egress to open-trips-archive.s3.amazonaws.com needs a person\'s approval before the first run.']
  const id = c.card({ type: 'proposal', workflow: doc, ...(check.workflow_hash ? { hash: check.workflow_hash } : {}), valid: check.ok, diagnostics: check.diagnostics, needs })
  await c.say(
    check.ok
      ? 'It validates. Two things to know before it runs: the fetch reaches a host, so the first run waits for an egress approval; and I left out any SQL or Python step on purpose, because code needs a human approval bound to its hash.'
      : 'It does not validate yet. The diagnostics are on the card; tell me what to change.',
  )
  if (!check.ok) return
  if (c.access === 'read') {
    await c.say(`This thread is ${ACCESS_INFO.read.label}, so I cannot store it. Press **Store** above, or raise my access to Author.`)
    return
  }
  step(3, 'doing')
  const put = await c.tool('workflows.put', { workflow: name }, () => c.backend.workflowsPut(doc, idempotencyKey('agent-put')), (p) => `${p.name} v${p.version}${p.created ? ' stored' : ' already stored'}`)
  step(3, 'done')
  if (put) {
    c.patch(id, (card) => {
      if (card.type === 'proposal') card.stored = { version: put.version }
    })
    await c.say(`Stored as [${put.name} v${put.version}](#/workflows/${put.name}). It has not run: the trigger fires at 01:30 UTC, and the first run will ask for the egress approval.`)
  }
}

const rebalance: Scenario = async (c) => {
  const runs = await c.tool('runs.list', { workflow: 'rebalance-plan', limit: 5 }, () => c.backend.runsList({ workflow: 'rebalance-plan', limit: 5 }), (r) => `${r.runs.length} runs of rebalance-plan`)
  const latest = (runs?.runs ?? []).find((r) => r.state === 'succeeded')
  if (!latest) {
    await c.say('There is no finished rebalance-plan run to read from yet.')
    return
  }
  const arts = await c.tool('artifacts.list', { run_id: latest.run_id }, () => c.backend.artifactsList(latest.run_id), (a) => `${a.artifacts.length} artifacts`)
  const csv = arts?.artifacts.find((a) => a.media_type === 'text/csv' && a.node === 'shortlist')
  if (!csv) {
    await c.say('The latest run has no shortlist.')
    return
  }
  const got = await c.tool('artifacts.read', { blake3: csv.blake3, max_bytes: 65_536 }, () => c.backend.artifactsGet(csv.blake3, 65_536), (g) => `${g.artifact.size_bytes} bytes, ${g.artifact.media_type}`)
  const text = got?.content_base64 ? asText(decodeBase64(got.content_base64)) : null
  const rows = text ? parseCsv(text, 400) : []
  const head = rows[0] ?? []
  const at = head.indexOf('expected_by')
  const body = rows.slice(1).filter((r) => r.length >= head.length && r[at]).sort((a, b) => String(a[at]).localeCompare(String(b[at])))
  const soon = body.slice(0, 8)
  await c.say(`**${body.length} stations are likely to run empty** in the shortlist of run [${latest.run_id.slice(0, 18)}…](#/runs/${latest.run_id}). The ${soon.length} that get there first:`)
  const pick = ['station_id', 'area', 'street', 'bikes', 'empty_risk_pct', 'expected_by']
  const idx = pick.map((h) => head.indexOf(h))
  c.card({
    type: 'table',
    title: 'Stations to rebalance, earliest first',
    columns: pick,
    rows: soon.map((r) => idx.map((i) => (i >= 0 ? (r[i] ?? '') : ''))),
    source: `artifact ${csv.blake3.slice(0, 12)}…, node ${csv.node}, run ${latest.run_id.slice(0, 18)}…`,
  })
  await c.say('The risk comes from thirty days of hourly dock rollups: how often a station was empty at the same hour of the day. Version 2 adds a weather adjustment, but that version is waiting for a person to approve its code, so these numbers come from version 1.')
}

const connectors: Scenario = async (c, text) => {
  const catalog = await loadCatalog()
  const words = text.toLowerCase().replace(FILLER, ' ').replace(/[^a-z0-9 ]+/g, ' ').split(/\s+/).filter((w) => w.length > 2)
  const vendors = ['datadog', 'dingtalk', 'slack', 'bigquery', 'snowflake', 'airflow', 'kafka', 's3', 'oss', 'postgres', 'feishu', 'wecom', 'grafana', 'splunk', 'dagster', 'prefect', 'databricks', 'redshift', 'clickhouse']
  const wanted = vendors.filter((v) => words.includes(v))
  const queries = wanted.length ? wanted : words.slice(0, 3)
  const found = await c.tool(
    'connectors.search',
    { queries },
    () => {
      const seen = new Map<string, (typeof catalog)[number]>()
      for (const q of queries) for (const m of catalog.filter((x) => matchesFilter(x, { query: q, category: '', region: '', maturity: '' })).slice(0, 2)) seen.set(m.id, m)
      return [...seen.values()].slice(0, 5)
    },
    (r) => `${r.length} of ${catalog.length} connectors`,
  )
  if (!found?.length) {
    await c.say('I found no connector that matches. The catalogue lists what Ogun can describe today; Settings, Connectors has a search with filters.')
    return
  }
  const manifestOnly = found.filter((m) => m.maturity === 'contract').length
  const built = found.length - manifestOnly
  await c.say(
    [
      `Here is what the catalogue says. I read manifests, not the services. ${built ? `${built} of these ${built === 1 ? 'is' : 'are'} **built**: the code exists and passes its own tests, checked against a local fake, never against the real service. ` : ''}${manifestOnly ? `${manifestOnly} ${manifestOnly === 1 ? 'is' : 'are'} **manifest only**: there is no code behind ${manifestOnly === 1 ? 'it' : 'them'} yet. ` : ''}None is verified.`,
      '',
      ...found.map((m) => `- **${m.name}** (${categoryLabel(m.category)}, ${MATURITY[m.maturity].label.toLowerCase()}): ${m.egress.domains.length ? `reaches ${m.egress.domains.slice(0, 2).map((d) => `\`${d}\``).join(', ')}, which needs an egress approval` : 'reaches no outside host'}. Secrets by name: ${m.auth[0]?.fields.filter((f) => f.secret).map((f) => `\`${f.name}\``).join(', ') || 'none'}.`),
    ].join('\n'),
  )
  for (const m of found.slice(0, 3)) c.card({ type: 'connector', connector_id: m.id, why: m.summary })
  await c.say('A connection holds the plain settings and the **name** of each secret. You store the value yourself with `ogun secrets set NAME` on a terminal; nothing in Studio or in this conversation can see or set it.')
}

const explain: Scenario = async (c, text) => {
  const wf = workflowIn(c, text)
  if (!wf) return help(c, text)
  const got = await c.tool('workflows.get', { name: wf }, () => c.backend.workflowsGet(wf), (g) => `${wf} v${g.version}, ${g.workflow.nodes.length} nodes`)
  if (!got) return
  const d = got.workflow
  const trig = ((d.triggers ?? []) as { kind: string; cron?: string }[]).map((t) => (t.kind === 'cron' ? `cron \`${t.cron}\`` : t.kind)).join(', ')
  const part = d.partitions as { kind: string; granularity?: string; start?: string } | undefined
  await c.say(
    [
      `**${d.workflow}** v${got.version}${d.description ? `: ${d.description}` : ''}`,
      '',
      `- Triggers: ${trig || 'none, it runs when someone starts it'}.`,
      `- Partitions: ${part ? (part.kind === 'time' ? `${part.granularity} from \`${part.start}\`` : part.kind) : 'one run, no partitions'}.`,
      `- Hash: \`${got.workflow_hash.slice(0, 16)}…\`. A run records this hash, so a result can always be traced to this exact document.`,
      '',
      '| Node | Operator | Reads |',
      '|---|---|---|',
      ...d.nodes.map((n) => `| \`${n.id}\` | \`${n.op}\`${n.code ? ' (code)' : ''} | ${n.inputs?.length ? n.inputs.map((i) => `\`${i}\``).join(', ') : 'nothing, it starts the graph'} |`),
      '',
      d.nodes.some((n) => n.code) ? 'Nodes marked code run sandboxed, and a change to their code needs a person\'s approval.' : 'These are all built-in operators: no code, so nothing here needs a code approval.',
    ].join('\n'),
  )
}

const help: Scenario = async (c, _text) => {
  await c.say(HELP)
}

function workflowIn(c: Ctx, text: string): string | undefined {
  const m = c.mentions.find((x) => x.kind === 'workflow')
  if (m) return m.id
  const lower = text.toLowerCase()
  const exact = c.names.find((n) => lower.includes(n))
  if (exact) return exact
  if (/ridership|forecast/.test(lower)) return c.names.find((n) => n === 'ridership-forecast')
  if (/trips|archive/.test(lower)) return c.names.find((n) => n === 'trips-backfill')
  if (/district|report/.test(lower)) return c.names.find((n) => n === 'district-report')
  if (/rebalanc|shortlist/.test(lower)) return c.names.find((n) => n === 'rebalance-plan')
  if (/telemetry|docks?\b/.test(lower)) return c.names.find((n) => n === 'dock-telemetry')
  return undefined
}

function route(text: string, c: Ctx): Scenario {
  const t = text.toLowerCase()
  if (workflowIn(c, text) && /\b(explain|what does|describe|how does|tell me about|walk me)\b/.test(t)) return explain
  if (/\b(fail|failed|failing|broke|broken|error|why did|debug|went wrong)\b/.test(t)) return failure
  if (/\b(backfill|catch up|since 20\d\d|missing (months|partitions)|re-?run all)\b/.test(t)) return backfill
  if (/\b(status|overview|health|what('| i)?s (running|happening|going on)|running|summary|summarise|summarize)\b/.test(t)) return status
  if (/\b(approv|waiting for (a )?person|publish request|code change)\w*/.test(t)) return approvalsScenario
  if (/\b(draft|create|write|build|make|new)\b.*\b(workflow|pipeline|job)\b/.test(t) || /\b(daily|hourly|weekly|monthly)\b.*\b(list|download|fetch|pull|ingest)\w*/.test(t)) return draft
  if (/\b(stations?|docks?|bikes?|rebalanc\w*)\b/.test(t) && /\b(empty|full|risk|rebalanc\w*|today|soon|run out|shortlist)\b/.test(t)) return rebalance
  if (/\b(connect|connector|connection|datadog|bigquery|snowflake|airflow|kafka|s3|oss|dingtalk|slack|feishu|wecom|grafana|splunk|dagster|prefect|databricks|redshift|clickhouse|alert|notify)\w*/.test(t)) return connectors
  if (workflowIn(c, text) && /\b(explain|what does|describe|how does|show|tell me about|walk)\b/.test(t)) return explain
  if (/^\s*(hi|hello|hey|help|\?)\W*$/.test(t)) return help
  if (workflowIn(c, text)) return explain
  return help
}

// ---------------------------------------------------------------- the agent

const MODELS: AgentModel[] = [
  { id: 'scripted', label: 'Ogun agent', provider: 'Ogun', available: true, note: 'Prepared answers in this build. No model is called.' },
  { id: 'anthropic-sonnet', label: 'Claude Sonnet 5.5', provider: 'Anthropic', available: false, note: 'Needs agent sessions on the daemon (O-17).' },
  { id: 'deepseek-chat', label: 'DeepSeek Chat', provider: 'DeepSeek', available: false, note: 'Needs agent sessions on the daemon (O-17).' },
  { id: 'qwen-max', label: 'Qwen Max', provider: 'Alibaba Cloud', available: false, note: 'Needs agent sessions on the daemon (O-17).' },
]

/** What a host keeps of the agent between restarts: the threads, the settings and the item counter. */
export interface AgentSnapshot {
  threads: AgentThread[]
  settings: AgentSettings
  seq: number
  seeded: boolean
}

/** How the agent is hosted: in the page (the offline build) or in the pre-alpha service, which persists it. */
export interface AgentHostOptions {
  /** Skip the word-by-word streaming and the pauses. Default: the reduced-motion preference. */
  instant?: () => boolean
  /** State saved by an earlier `snapshot()`. */
  restore?: AgentSnapshot
  /** Called after every change, so a host can save `snapshot()`. */
  onChange?: () => void
  /** Seed two finished threads on first use (default true). */
  seed?: boolean
}

/** The agent with what a host needs to persist it. */
export interface HostedAgent extends AgentApi {
  snapshot(): AgentSnapshot
  /** A card's button pressed by a person; `backend` acts with that person's own credential when given. */
  act(threadId: string, itemId: string, action: AgentAction, idempotencyKey: string, backend?: Backend): Promise<void>
}

export function createScriptedAgent(getBackend: () => Backend, opts: AgentHostOptions = {}): HostedAgent {
  const threads = new Map<string, Live>()
  const listListeners = new Set<(t: AgentThreadSummary[]) => void>()
  const threadListeners = new Map<string, Set<(t: AgentThread | null) => void>>()
  let seq = opts.restore?.seq ?? 0
  let seeded = opts.restore?.seeded ?? opts.seed === false
  let settings: AgentSettings = {
    instructions: DEFAULT_INSTRUCTIONS,
    default_access: 'read',
    default_model: 'scripted',
    spend_cap_usd: 2,
    tools_enabled: TOOLS.map((t) => t.name),
    retention_days: 30,
    // The scripted agent calls no model, so these do not change what it does; they keep the settings page whole.
    offline: false,
    consented_providers: [],
    day_cap_usd: 10,
    // A snapshot saved before these members existed still loads.
    ...(opts.restore?.settings ?? {}),
  }
  for (const t of opts.restore?.threads ?? []) {
    // A turn that was running when the host stopped is over: nothing resumes it.
    if (t.state === 'running') t.state = 'idle'
    for (const it of t.items) {
      if (it.kind === 'assistant') it.streaming = false
      if (it.kind === 'tool' && it.state === 'running') it.state = 'error'
    }
    threads.set(t.thread_id, { thread: t, stop: false, running: false })
  }
  const changed = () => opts.onChange?.()

  const summary = (l: Live): AgentThreadSummary => {
    const { items, mentions: _m, ...rest } = l.thread
    return { ...rest, item_count: items.length }
  }
  const emitList = () => {
    const all = [...threads.values()].map(summary).sort((a, b) => b.updated_at.localeCompare(a.updated_at))
    for (const f of listListeners) f(all)
    changed()
  }
  const emit = (l: Live) => {
    l.thread.updated_at = iso(Date.now())
    const set = threadListeners.get(l.thread.thread_id)
    if (set) {
      const snap = clone(l.thread)
      for (const f of set) f(snap)
    }
    emitList()
  }
  const nid = () => `itm_${(++seq).toString(36).padStart(4, '0')}`
  const push = (l: Live, item: Omit<AgentItem, 'id' | 'at'> & { kind: AgentItem['kind'] }): AgentItem => {
    const full = { ...item, id: nid(), at: iso(Date.now()) } as AgentItem
    l.thread.items.push(full)
    return full
  }

  const instant = opts.instant ?? (() => getPrefs().reduceMotion)

  function ctxFor(l: Live, names: string[], mentions: Mention[], fast: boolean): Ctx {
    const guard = () => {
      if (l.stop) throw new Stopped()
    }
    const wait = async (ms: number) => {
      guard()
      if (!fast && !instant()) await sleep(ms)
      guard()
    }
    return {
      access: l.thread.access,
      backend: getBackend(),
      mentions,
      names,
      pause: wait,
      async say(text) {
        const item = push(l, { kind: 'assistant', text: '', streaming: true } as AgentItem) as Extract<AgentItem, { kind: 'assistant' }>
        const paragraphs = text.split('\n')
        for (let p = 0; p < paragraphs.length; p++) {
          const line = (paragraphs[p] as string) + (p < paragraphs.length - 1 ? '\n' : '')
          if (fast || instant()) {
            item.text += line
            continue
          }
          for (let i = 0; i < line.length; i += 5) {
            guard()
            item.text += line.slice(i, i + 5)
            emit(l)
            await sleep(12)
          }
        }
        item.streaming = false
        emit(l)
      },
      card(card) {
        const item = push(l, { kind: 'card', card } as AgentItem)
        emit(l)
        return item.id
      },
      patch(id, f) {
        const item = l.thread.items.find((x) => x.id === id)
        if (item && item.kind === 'card') {
          f(item.card)
          emit(l)
        }
      },
      notice(text, tone = 'info') {
        push(l, { kind: 'notice', tone, text } as AgentItem)
        emit(l)
      },
      async tool<T>(name: string, input: unknown, run: () => Promise<T> | T, summarize: (out: T) => string): Promise<T | undefined> {
        guard()
        const allowed = mayCall(name, l.thread.access, settings.tools_enabled)
        if (!allowed.ok) {
          push(l, { kind: 'tool', tool: name, summary: `refused: ${allowed.reason}`, input, state: 'denied' } as AgentItem)
          emit(l)
          return undefined
        }
        const item = push(l, { kind: 'tool', tool: name, summary: 'running…', input, state: 'running' } as AgentItem) as Extract<AgentItem, { kind: 'tool' }>
        emit(l)
        const t0 = Date.now()
        await wait(260 + Math.floor(Math.random() * 280))
        try {
          const out = await run()
          item.state = 'ok'
          item.summary = summarize(out)
          item.output = bounded(out)
          item.ms = Date.now() - t0
          emit(l)
          return out
        } catch (e) {
          const err = asOgunError(e)
          item.state = 'error'
          item.summary = err.message
          item.ms = Date.now() - t0
          emit(l)
          return undefined
        }
      },
    }
  }

  async function turn(l: Live, text: string, mentions: Mention[], fast = false): Promise<void> {
    const backend = getBackend()
    l.running = true
    l.stop = false
    l.thread.state = 'running'
    if (l.thread.items.length === 0) l.thread.title = titleOf(text)
    for (const m of mentions) if (!l.thread.mentions.some((x) => x.kind === m.kind && x.id === m.id)) l.thread.mentions.push(m)
    push(l, { kind: 'user', text, mentions } as AgentItem)
    emit(l)
    try {
      const names = (await backend.workflowsList().catch(() => ({ workflows: [] }))).workflows.map((w) => w.name)
      const c = ctxFor(l, names, mentions, fast)
      await c.pause(fast ? 0 : 350)
      await route(text, c)(c, text)
    } catch (e) {
      if (e instanceof Stopped) {
        const last = l.thread.items[l.thread.items.length - 1]
        if (last && last.kind === 'assistant') last.streaming = false
        for (const it of l.thread.items) if (it.kind === 'tool' && it.state === 'running') it.state = 'error'
        push(l, { kind: 'notice', tone: 'info', text: 'Stopped. Nothing further was done.' } as AgentItem)
      } else {
        push(l, { kind: 'notice', tone: 'warn', text: `The turn failed: ${asOgunError(e).message}` } as AgentItem)
      }
    } finally {
      l.running = false
      const pendingCard = l.thread.items.some((it) => it.kind === 'card' && ((it.card.type === 'proposal' && !it.card.stored && it.card.valid) || (it.card.type === 'backfill' && !it.card.started && it.card.to_run > 0) || (it.card.type === 'retry' && !it.card.started)))
      l.thread.state = pendingCard ? 'waiting' : 'idle'
      emit(l)
    }
  }

  function create(access: AgentAccess, model: string): Live {
    const now = Date.now()
    const l: Live = {
      thread: {
        thread_id: 'thr_' + ulid(now, 'thr:' + ++seq + ':' + now),
        title: 'New thread',
        state: 'idle',
        access,
        model,
        created_at: iso(now),
        updated_at: iso(now),
        item_count: 0,
        items: [],
        mentions: [],
      },
      stop: false,
      running: false,
    }
    threads.set(l.thread.thread_id, l)
    return l
  }

  /** Two finished threads, so the sidebar and the thread view are alive on first load. */
  async function seed(): Promise<void> {
    if (seeded) return
    seeded = true
    const back = (l: Live, ms: number) => {
      const shift = (ts: string) => iso(Date.parse(ts) - ms)
      l.thread.created_at = shift(l.thread.created_at)
      l.thread.updated_at = shift(l.thread.updated_at)
      for (const it of l.thread.items) it.at = shift(it.at)
    }
    const a = create('read', 'scripted')
    await turn(a, 'Why did the ridership forecast fail last night?', [{ kind: 'workflow', id: 'ridership-forecast', label: 'ridership-forecast' }], true)
    back(a, 3 * 3_600_000)
    const b = create('read', 'scripted')
    await turn(b, 'What is waiting for a person?', [], true)
    back(b, 26 * 3_600_000)
    emitList()
  }

  return {
    snapshot: () => ({ threads: [...threads.values()].map((l) => clone(l.thread)), settings: clone(settings), seq, seeded }),
    models: async () => clone(MODELS),
    tools: async () => clone(TOOLS),
    settings: async () => clone(settings),
    setSettings: async (patch) => {
      settings = { ...settings, ...clone(patch) }
      changed()
      return clone(settings)
    },
    watchThreads(onChange) {
      listListeners.add(onChange)
      void seed().then(() => emitList())
      onChange([...threads.values()].map(summary).sort((a, b) => b.updated_at.localeCompare(a.updated_at)))
      return { close: () => void listListeners.delete(onChange) } satisfies EventSubscription
    },
    watchThread(threadId, onChange) {
      let set = threadListeners.get(threadId)
      if (!set) threadListeners.set(threadId, (set = new Set()))
      set.add(onChange)
      void seed().then(() => {
        const l = threads.get(threadId)
        onChange(l ? clone(l.thread) : null)
      })
      const l = threads.get(threadId)
      if (l) onChange(clone(l.thread))
      return { close: () => void set.delete(onChange) }
    },
    async create({ access, model }) {
      const l = create(access, model)
      emitList()
      return clone(l.thread)
    },
    async send(threadId, text, mentions) {
      const l = threads.get(threadId)
      if (!l) throw asOgunError({ code: 'api.not_found', class: 'not_found', message: 'no such thread' })
      if (l.running) throw asOgunError({ code: 'api.conflict', class: 'conflict', message: 'the agent is still working on the last message' })
      void turn(l, text, mentions)
    },
    async stop(threadId) {
      const l = threads.get(threadId)
      if (l) l.stop = true
    },
    async act(threadId, itemId, action: AgentAction, key, asPerson?: Backend) {
      const l = threads.get(threadId)
      const item = l?.thread.items.find((x) => x.id === itemId)
      if (!l || !item || item.kind !== 'card') throw asOgunError({ code: 'api.not_found', class: 'not_found', message: 'no such card' })
      const backend = asPerson ?? getBackend()
      const card = item.card
      if (action === 'store' && card.type === 'proposal') {
        if (card.stored) return
        const put = await backend.workflowsPut(card.workflow, key)
        card.stored = { version: put.version }
        const approvals = put.approvals.length ? ` ${put.approvals.length} approval${put.approvals.length === 1 ? '' : 's'} now wait for a person.` : ''
        push(l, { kind: 'assistant', text: `Stored as [${put.name} v${put.version}](#/workflows/${put.name}) by you, not by me.${approvals}`, streaming: false } as AgentItem)
      } else if (action === 'start_backfill' && card.type === 'backfill') {
        if (card.started) return
        const run = await backend.runsStart({ workflow: card.workflow, partitions: { kind: 'range', from: card.from, to: card.to }, idempotency_key: key })
        card.started = { run_id: run.run_id }
        push(l, { kind: 'card', card: { type: 'run', run_id: run.run_id, workflow: card.workflow } } as AgentItem)
        push(l, { kind: 'assistant', text: 'Started by you. The run is idempotent: pressing the button again returns this run, not a second one.', streaming: false } as AgentItem)
      } else if (action === 'retry' && card.type === 'retry') {
        if (card.started) return
        const failed = await backend.runsGet(card.run_id)
        const partition = failed.nodes?.find((n) => n.node === card.node && n.state === 'failed')?.partition
        if (!partition) throw asOgunError({ code: 'api.conflict', class: 'conflict', message: 'that node is no longer failed' })
        const run: RunView = await backend.runsStart({ workflow: card.workflow, partitions: { kind: 'keys', keys: [partition] }, idempotency_key: key })
        card.started = { run_id: run.run_id }
        push(l, { kind: 'card', card: { type: 'run', run_id: run.run_id, workflow: card.workflow } } as AgentItem)
        push(l, { kind: 'assistant', text: `Started by you for partition \`${partition}\`. Finished nodes of the failed run are not run again.`, streaming: false } as AgentItem)
      } else {
        throw asOgunError({ code: 'api.invalid_params', class: 'invalid_argument', message: 'that action does not belong to this card' })
      }
      emit(l)
      l.thread.state = l.thread.items.some((it) => it.kind === 'card' && ((it.card.type === 'proposal' && !it.card.stored && it.card.valid) || (it.card.type === 'backfill' && !it.card.started && it.card.to_run > 0) || (it.card.type === 'retry' && !it.card.started))) ? 'waiting' : 'idle'
      emit(l)
    },
    async setAccess(threadId, access) {
      const l = threads.get(threadId)
      if (!l) return
      if (l.thread.access !== access) {
        l.thread.access = access
        push(l, { kind: 'notice', tone: 'info', text: `Access is now ${ACCESS_INFO[access].label}. ${ACCESS_INFO[access].blurb}` } as AgentItem)
        emit(l)
      }
    },
    async rename(threadId, title) {
      const l = threads.get(threadId)
      if (l && title.trim()) {
        l.thread.title = title.trim().slice(0, 80)
        emit(l)
      }
    },
    async remove(threadId) {
      threads.delete(threadId)
      const set = threadListeners.get(threadId)
      if (set) for (const f of set) f(null)
      emitList()
    },
  }
}
