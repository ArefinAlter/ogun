// The scripted backend: a bike-share network's example workflows on a daemon that exists only in this browser tab
// (or this window). Every view reads it through the same `Backend` interface as a real `ogund`, so the pages are the
// real pages; only the data is a script. It is chosen only by `VITE_OGUN_DEMO=1`, `VITE_OGUN_DESKTOP_DEMO=1` or
// `?demo` in the URL, never inside a normal Tauri shell, and it loads as its own chunk.
//
// What the script contains: a 93-month trip-archive backfill in progress with one throttled retry, the dock
// telemetry stream sealing a window every 30 s, a week of daily rebalance plans, a monthly district report waiting
// for a publish approval, a failed ridership forecast, and the rebalance-plan v2 code change waiting for approval.
// Two controls (`backend.demo`, reached from the command palette) replay the two claims the O1 exit test proves: a
// `kill -9` of the daemon that loses nothing, and late station data that becomes a new revision instead of a gap.
//
// Workflow documents and their hashes come from `./docs` (generated from `examples/bikeshare` by
// `scripts/sync-sample-docs.mjs`; the hashes are what `ogun hash` prints). Everything else is invented here.

import type { Backend, DemoControls, EventSubscription, PreparedDecision, ShellInfo } from '../api/backend'
import { asOgunError } from '../api/backend'
import type {
  ApprovalKind,
  ApprovalState,
  ApprovalView,
  ArtifactView,
  CredentialView,
  Decision,
  Diagnostic,
  EdgeView,
  EventView,
  GetArtifactResult,
  GetWorkflowResult,
  LadderResult,
  LineageResult,
  ListApprovalsResult,
  ListArtifactsResult,
  ListRunsParams,
  ListRunsResult,
  ListWorkflowsResult,
  NodeState,
  NodeView,
  OgunError,
  PartitionRange,
  PartitionState,
  PartitionStatusParams,
  PartitionStatusResult,
  PlanBackfillResult,
  PutWorkflowResult,
  RunView,
  StartRunParams,
  StatusResult,
  StreamView,
  ValidateResult,
  WorkflowDoc,
  WorkflowNodeDoc,
  WorkflowSummary,
} from '../api/types'
import { createScriptedAgent } from './agent'
import { DemoExtras } from './extras'
import { DOCS, HASHES } from './docs'
import {
  B32,
  DAY,
  HOUR,
  MIN,
  RESTART_MS,
  SEC,
  TERMINAL,
  TICK_MS,
  WINDOW_EVERY_MS,
  clone,
  compress,
  dayKeys,
  dayOf,
  err,
  fnv,
  hex,
  iso,
  monthKeys,
  monthOf,
  pad2,
  rng,
  sleep,
  ulid,
  windowKey,
} from './util'

// ---------------------------------------------------------------- the scenario

export interface Version {
  version: number
  hash: string
  doc: WorkflowDoc
  runnable: boolean
  storedAt: number
}

export interface Workflow {
  id: string
  name: string
  enabled: boolean
  versions: Version[]
}

export interface Run {
  view: RunView
  doc: WorkflowDoc
  instances: NodeView[]
  partitions: string[]
  concurrency: number
  /** Probability per tick that a running instance finishes. */
  speed: number
  createdAt: number
  /** One scripted failure that retries once. */
  script?: { node: string; partition: string; cause: string; done: boolean }
}

interface Listener {
  runId?: string
  onEvent: (e: EventView) => void
}

interface SealedWindow {
  key: string
  revision: number
  records: number
  blake3: string
}

interface StreamState {
  sealed: number
  late: number
  invalid: number
  dropped: number
  buffered: number
  nextSealAt: number
  watermarkMs: number
  open: { key: string; records: number; closesAt: number }
  recent: SealedWindow[]
}

const SITES = [
  ['RIV', 'Riverside'],
  ['HAR', 'Harbor'],
  ['OLD', 'Old Town'],
  ['UNI', 'University'],
  ['MKT', 'Market Square'],
  ['EAS', 'Eastside'],
] as const

const KNOWN_OPS = new Set([
  'acquire@1',
  'archive.extract@1',
  'hash@1',
  'dedupe@1',
  'manifest@1',
  'file.copy@1',
  'file.move@1',
  'crawl@1',
  'filter@1',
  'window.rollup@1',
  'stream.mqtt@1',
  'stream.http@1',
  'duckdb@1',
  'sql@1',
  'python@1',
  'dbt@1',
  'pandoc@1',
  'llm.extract@1',
  'llm.classify@1',
  'embedding.generate@1',
])

const SECRETISH = /^(password|passwd|token|api_key|apikey|secret|authorization|cookie)$/i

function producers(doc: WorkflowDoc, nodeId: string): string[] {
  const n = doc.nodes.find((x) => x.id === nodeId)
  const ids = new Set(doc.nodes.map((x) => x.id))
  return (n?.inputs ?? []).map((i) => i.split('.')[0] ?? i).filter((id) => ids.has(id))
}

function docOf(key: string): WorkflowDoc {
  const d = DOCS[key]
  if (!d) throw new Error(`demo: no document ${key}`)
  return clone(d)
}

function hashOf(key: string): string {
  const h = HASHES[key]
  if (!h) throw new Error(`demo: no hash ${key}`)
  return h
}

function granularity(name: string): 'month' | 'day' {
  return name === 'trips-backfill' || name === 'district-report' ? 'month' : 'day'
}

function keysBetween(name: string, from: string, to: string): string[] {
  return granularity(name) === 'month' ? monthKeys(from.slice(0, 7), to.slice(0, 7)) : dayKeys(from.slice(0, 10), to.slice(0, 10))
}

class Demo {
  readonly startedAtFirst = Date.now()
  startedAt: number
  down = false
  seq = 48_213
  private readonly listeners = new Set<Listener>()
  private readonly recent: EventView[] = []
  readonly workflows: Workflow[] = []
  readonly runs = new Map<string, Run>()
  readonly approvals = new Map<string, ApprovalView>()
  readonly stream: StreamState
  private readonly artifactsByRun = new Map<string, ArtifactView[]>()
  private readonly artifactContent = new Map<string, string>()
  private readonly idempotent = new Map<string, string>()
  readonly edges: EdgeView[]
  readonly extras: DemoExtras
  readonly credentials: CredentialView[]

  constructor() {
    const now = Date.now()
    this.startedAt = now - 3 * HOUR - 12 * MIN

    // ---- workflows
    const wf = (name: string, versions: [number, number, boolean][]) => {
      const w: Workflow = {
        id: 'wf_' + ulid(now - 30 * DAY, 'wf:' + name),
        name,
        enabled: true,
        versions: versions.map(([v, ago, runnable]) => ({
          version: v,
          hash: hashOf(`${name}@${v}`),
          doc: docOf(`${name}@${v}`),
          runnable,
          storedAt: now - ago,
        })),
      }
      this.workflows.push(w)
      return w
    }
    wf('dock-telemetry', [[1, 13 * DAY, true]])
    wf('rebalance-plan', [
      [1, 13 * DAY, true],
      [2, 25 * MIN, false],
    ])
    wf('trips-backfill', [[1, 12 * DAY, true]])
    wf('district-report', [[1, 9 * DAY, true]])
    wf('ridership-forecast', [[1, 6 * DAY, true]])

    // ---- the backfill in progress: 93 months, 84 done, 3 in flight, one throttled retry to come
    const backfill = this.newRun('trips-backfill', 1, monthKeys('2019-01', '2026-09'), now - 41 * MIN, 'backfill', 4, 0.45)
    backfill.script = { node: 'fetch', partition: '2026-02', cause: 'http.throttled', done: false }
    {
      const step = (39 * MIN) / (84 * 4)
      let k = 0
      for (const inst of backfill.instances) {
        const idx = backfill.partitions.indexOf(inst.partition)
        if (idx < 84) {
          inst.state = 'succeeded'
          inst.attempt = 1
          inst.updated_at = iso(backfill.createdAt + k++ * step)
        }
      }
      const set = (partition: string, node: string, state: NodeState) => {
        const i = this.instance(backfill, node, partition)
        if (i) {
          i.state = state
          i.attempt = 1
          i.updated_at = iso(now - 20 * SEC)
        }
      }
      set('2026-01', 'stations', 'succeeded')
      set('2026-01', 'fetch', 'succeeded')
      set('2026-01', 'daily', 'running')
      set('2026-02', 'stations', 'succeeded')
      set('2026-02', 'fetch', 'running')
      set('2026-03', 'stations', 'running')
    }

    // ---- the dock telemetry stream: a window a minute, sealed; recent runs succeeded
    const sealedBefore = 1842
    const recent: SealedWindow[] = []
    for (let i = 4; i >= 1; i--) {
      const at = now - i * MIN
      const key = windowKey(at - MIN)
      const records = 96 + (fnv(key) % 40)
      recent.unshift({ key, revision: 1, records, blake3: hex('window:' + key) })
      const r = this.newRun('dock-telemetry', 1, [key], at + 2 * SEC, 'stream', 3, 1)
      this.finish(r, 'succeeded', at + 9 * SEC)
    }
    this.stream = {
      sealed: sealedBefore + recent.length,
      late: 12,
      invalid: 0,
      dropped: 0,
      buffered: 64,
      nextSealAt: now + 9 * SEC,
      watermarkMs: now - 75 * SEC,
      open: { key: windowKey(now), records: 41, closesAt: now + 9 * SEC },
      recent,
    }

    // ---- daily rebalance-plan runs, a week of them
    for (let d = 6; d >= 0; d--) {
      const base = new Date(iso(now - d * DAY).slice(0, 10) + 'T00:45:00.000Z').getTime()
      if (base > now) continue
      const r = this.newRun('rebalance-plan', 1, [dayOf(base)], base, 'cron', 3, 1)
      this.finish(r, 'succeeded', base + 4 * MIN)
    }

    // ---- the September district report, waiting for a person
    const reportAt = new Date('2026-10-02T03:00:00.000Z').getTime()
    const report = this.newRun('district-report', 1, ['2026-09'], reportAt, 'cron', 4, 1)
    for (const n of ['hourly', 'figures', 'render']) this.setState(report, n, '2026-09', 'succeeded', reportAt + 6 * MIN)
    this.setState(report, 'deliver', '2026-09', 'waiting_for_user', reportAt + 7 * MIN)
    report.view.state = 'waiting'

    // ---- last night's ridership forecast, failed on the forecast source
    const fcAt = new Date('2026-10-23T23:20:00.000Z').getTime()
    const fc = this.newRun('ridership-forecast', 1, [dayOf(fcAt)], fcAt, 'cron', 4, 1)
    this.setState(fc, 'trips', dayOf(fcAt), 'succeeded', fcAt + 40 * SEC)
    const f = this.instance(fc, 'forecast', dayOf(fcAt))
    if (f) {
      f.state = 'failed'
      f.attempt = 3
      f.last_cause = 'http.timeout'
      f.updated_at = iso(fcAt + 6 * MIN)
    }
    this.setState(fc, 'model', dayOf(fcAt), 'skipped', fcAt + 6 * MIN)
    this.setState(fc, 'publish', dayOf(fcAt), 'skipped', fcAt + 6 * MIN)
    fc.view.state = 'failed'

    // ---- approvals
    const v1 = DOCS['rebalance-plan@1']?.nodes.find((n) => n.id === 'estimate')?.code?.blake3 ?? ''
    const v2 = DOCS['rebalance-plan@2']?.nodes.find((n) => n.id === 'estimate')?.code?.blake3 ?? ''
    this.addApproval({
      kind: 'code_change',
      state: 'pending',
      workflow: 'rebalance-plan',
      workflow_hash: hashOf('rebalance-plan@2'),
      node: 'estimate',
      facts: [
        { label: 'operator', value: 'python@1' },
        { label: 'code, new', value: v2 },
        { label: 'code, previous', value: v1 },
        { label: 'path', value: 'examples/bikeshare/ops/rebalance_weather.py' },
        { label: 'changed since v1', value: 'nodes.estimate.code, nodes.estimate.config' },
        { label: 'sandbox', value: 'bubblewrap, no network, wall 600 000 ms' },
      ],
      requested_by: 'ogun-mcp (agent credential)',
      created_at: iso(now - 25 * MIN),
      expires_at: iso(now - 25 * MIN + DAY),
      untrusted_reason:
        'Added a weather adjustment to the rebalance model: on a wet or cold day the evening peak counts for less, so stations near the river are flagged later in the day than the stations by the university.',
    })
    this.addApproval({
      kind: 'publish',
      state: 'pending',
      workflow: 'district-report',
      workflow_hash: hashOf('district-report@1'),
      run_id: report.view.run_id,
      node: 'deliver',
      facts: [
        { label: 'partition', value: '2026-09' },
        { label: 'documents', value: '12 PDF (en, es), 2.4 MiB' },
        { label: 'destination', value: 'outbox/districts/2026-09/' },
        { label: 'recipients', value: '6 district offices, 38 addresses' },
        { label: 'policy', value: 'approval: always' },
      ],
      requested_by: 'scheduler (cron 0 3 2 * *)',
      created_at: iso(reportAt + 7 * MIN),
      expires_at: iso(reportAt + 7 * MIN + 30 * DAY),
    })
    this.addApproval({
      kind: 'egress',
      state: 'approved',
      workflow: 'trips-backfill',
      workflow_hash: hashOf('trips-backfill@1'),
      node: 'fetch',
      facts: [
        { label: 'allowed domains', value: 'open-trips-archive.s3.amazonaws.com' },
        { label: 'listing', value: 's3_prefix' },
        { label: 'budget', value: '2 GiB, wall 7 200 000 ms' },
      ],
      requested_by: 'operator (terminal)',
      created_at: iso(now - 3 * DAY),
      expires_at: iso(now - 2 * DAY),
      decided_at: iso(now - 3 * DAY + 4 * MIN),
      decided_via: 'terminal',
      decided_by: 'operator',
    })
    this.addApproval({
      kind: 'budget',
      state: 'denied',
      workflow: 'ridership-forecast',
      workflow_hash: hashOf('ridership-forecast@1'),
      facts: [
        { label: 'requested', value: 'wall 7 200 000 ms' },
        { label: 'policy', value: 'wall 600 000 ms' },
      ],
      requested_by: 'ogun-mcp (agent credential)',
      created_at: iso(now - 5 * DAY),
      expires_at: iso(now - 4 * DAY),
      decided_at: iso(now - 5 * DAY + 11 * MIN),
      decided_via: 'studio',
      decided_by: 'operator',
      untrusted_reason: 'The weather endpoint is slow on Fridays; raising the budget avoids the timeout.',
    })
    this.addApproval({
      kind: 'egress',
      state: 'expired',
      workflow: 'ridership-forecast',
      workflow_hash: hashOf('ridership-forecast@1'),
      node: 'forecast',
      facts: [{ label: 'allowed domains', value: 'archive-api.open-meteo.com' }],
      requested_by: 'studio import',
      created_at: iso(now - 9 * DAY),
      expires_at: iso(now - 8 * DAY),
    })

    // ---- edges and credentials
    this.edges = [
      {
        edge_id: 'edg_' + ulid(now - 4 * DAY, 'edge:pixel'),
        name: 'Eastside depot gateway (Pixel 7a)',
        public_key: hex('pk:pixel'),
        enrolled_at: iso(now - 4 * DAY),
        last_sync_at: iso(now - 4 * MIN),
      },
      {
        edge_id: 'edg_' + ulid(now - 11 * DAY, 'edge:riverside'),
        name: 'Riverside dock gateway',
        public_key: hex('pk:riverside'),
        enrolled_at: iso(now - 11 * DAY),
        last_sync_at: iso(now - 62 * MIN),
      },
    ]
    const cred = (name: string, kind: CredentialView['kind'], ago: number, revoked = false): CredentialView => ({
      credential_id: 'ocl_' + ulid(now - ago, 'cred:' + name),
      kind,
      name,
      created_at: iso(now - ago),
      revoked,
    })
    this.credentials = [
      cred('studio', 'human', 13 * DAY),
      cred('cli', 'human', 13 * DAY),
      cred('ogun-mcp', 'agent', 12 * DAY),
      cred('zeus-bridge', 'service', 9 * DAY),
      cred('hub-relay', 'bridge', 6 * DAY),
      cred('ogun-mcp-old', 'agent', 20 * DAY, true),
    ]

    this.extras = new DemoExtras({
      workflows: () => this.workflows,
      runs: () => this.runs,
      put: (w, k) => this.put(w, k),
      guard: () => this.guard(),
      notify: (kind) => this.emit('daemon', kind),
    })
    setInterval(() => this.tick(), TICK_MS)
  }

  // ---------------------------------------------------------------- state helpers

  private workflow(name: string): Workflow {
    const w = this.workflows.find((x) => x.name === name)
    if (!w) throw err('workflow.not_found', 'not_found', `no workflow named ${name}`)
    return w
  }

  private version(w: Workflow, version?: number): Version {
    const v = version === undefined ? w.versions[w.versions.length - 1] : w.versions.find((x) => x.version === version)
    if (!v) throw err('workflow.version_not_found', 'not_found', `${w.name} has no version ${version}`)
    return v
  }

  private summary(w: Workflow): WorkflowSummary {
    const latest = w.versions[w.versions.length - 1]
    if (!latest) throw err('internal.bug', 'internal', 'workflow without versions')
    return {
      workflow_id: w.id,
      name: w.name,
      latest_version: latest.version,
      workflow_hash: latest.hash,
      enabled: w.enabled,
      runnable: latest.runnable,
      updated_at: iso(latest.storedAt),
      ...(latest.doc.description ? { untrusted_description: latest.doc.description } : {}),
    }
  }

  private newRun(
    name: string,
    version: number,
    partitions: string[],
    createdAt: number,
    seed: string,
    concurrency: number,
    speed: number,
  ): Run {
    const w = this.workflow(name)
    const v = this.version(w, version)
    const runId = 'orun_' + ulid(createdAt, `run:${name}:${version}:${seed}:${partitions[0] ?? ''}`)
    const instances: NodeView[] = []
    for (const p of partitions) {
      for (const n of v.doc.nodes) {
        if (n.op.startsWith('stream.')) continue
        instances.push({
          instance_id: 'ni_' + ulid(createdAt, `ni:${runId}:${n.id}:${p}`),
          node: n.id,
          partition: p,
          state: 'pending',
          attempt: 0,
          updated_at: iso(createdAt),
        })
      }
    }
    const run: Run = {
      view: {
        run_id: runId,
        workflow: name,
        version,
        workflow_hash: v.hash,
        state: 'running',
        partition_count: partitions.length,
        created_at: iso(createdAt),
      },
      doc: v.doc,
      instances,
      partitions,
      concurrency,
      speed,
      createdAt,
    }
    this.runs.set(runId, run)
    return run
  }

  private instance(run: Run, node: string, partition: string): NodeView | undefined {
    return run.instances.find((i) => i.node === node && i.partition === partition)
  }

  private setState(run: Run, node: string, partition: string, state: NodeState, at: number): void {
    const i = this.instance(run, node, partition)
    if (!i) return
    i.state = state
    i.attempt = Math.max(i.attempt, 1)
    i.updated_at = iso(at)
  }

  private finish(run: Run, state: 'succeeded' | 'failed', at: number): void {
    for (const i of run.instances) {
      i.state = state
      i.attempt = 1
      i.updated_at = iso(at)
    }
    run.view.state = state
  }

  private addApproval(a: Omit<ApprovalView, 'approval_id' | 'request_hash'>): ApprovalView {
    const created = new Date(a.created_at).getTime()
    const id = 'oap_' + ulid(created, `ap:${a.kind}:${a.workflow}:${a.created_at}`)
    const view: ApprovalView = { ...a, approval_id: id, request_hash: hex('req:' + id) }
    this.approvals.set(id, view)
    return view
  }

  private emit(runId: string, kind: Record<string, unknown>, instanceId?: string): void {
    const e: EventView = { seq: ++this.seq, at: iso(Date.now()), run_id: runId, ...(instanceId ? { instance_id: instanceId } : {}), kind }
    this.recent.push(e)
    if (this.recent.length > 500) this.recent.shift()
    for (const l of this.listeners) if (!l.runId || l.runId === runId) l.onEvent(clone(e))
  }

  private transition(run: Run, inst: NodeView, to: NodeState, cause?: string): void {
    const from = inst.state
    inst.state = to
    inst.updated_at = iso(Date.now())
    if (cause) inst.last_cause = cause
    this.emit(
      run.view.run_id,
      { type: 'node', node: inst.node, partition: inst.partition, from, to, attempt: inst.attempt, ...(cause ? { cause } : {}) },
      inst.instance_id,
    )
  }

  // ---------------------------------------------------------------- the clock

  private tick(): void {
    if (this.down) return
    const now = Date.now()
    for (const run of this.runs.values()) if (run.view.state === 'running') this.advance(run)
    const s = this.stream
    if (now >= s.nextSealAt) this.seal(now)
    else {
      s.open.records += 2 + (fnv(String(now)) % 5)
      s.buffered = 40 + (fnv(String(now >> 10)) % 90)
      s.watermarkMs = now - 60 * SEC - (fnv(String(now >> 12)) % 30) * SEC
    }
  }

  private advance(run: Run): void {
    const roll = Math.random
    for (const inst of run.instances) {
      if (inst.state !== 'running') continue
      if (roll() >= run.speed) continue
      const sc = run.script
      if (sc && !sc.done && inst.node === sc.node && inst.partition === sc.partition) {
        sc.done = true
        this.transition(run, inst, 'retry_scheduled', sc.cause)
      } else this.transition(run, inst, 'succeeded')
    }
    for (const inst of run.instances) {
      if (inst.state === 'retry_scheduled') {
        inst.attempt++
        this.transition(run, inst, 'running')
      }
    }
    let active = run.instances.filter((i) => i.state === 'running').length
    for (const inst of run.instances) {
      if (inst.state !== 'pending') continue
      const prods = producers(run.doc, inst.node).map((p) => this.instance(run, p, inst.partition))
      if (prods.some((p) => p && (p.state === 'failed' || p.state === 'cancelled' || p.state === 'skipped'))) {
        this.transition(run, inst, 'skipped', 'producer_failed')
        continue
      }
      if (active >= run.concurrency) continue
      if (prods.every((p) => !p || p.state === 'succeeded')) {
        inst.attempt = 1
        active++
        this.transition(run, inst, 'running')
      }
    }
    if (run.instances.every((i) => TERMINAL.has(i.state))) {
      const failed = run.instances.some((i) => i.state === 'failed')
      run.view.state = failed ? 'failed' : 'succeeded'
      this.emit(run.view.run_id, { type: 'run', state: run.view.state, partitions: run.partitions.length, workflow: run.view.workflow })
    } else if (run.instances.some((i) => i.state === 'waiting_for_user')) run.view.state = 'waiting'
  }

  private seal(now: number): void {
    const s = this.stream
    const key = s.open.key
    const records = s.open.records
    s.recent.unshift({ key, revision: 1, records, blake3: hex('window:' + key + ':1') })
    if (s.recent.length > 12) s.recent.pop()
    s.sealed++
    s.nextSealAt = now + WINDOW_EVERY_MS
    s.open = { key: windowKey(now), records: 0, closesAt: now + WINDOW_EVERY_MS }
    s.buffered = Math.max(0, s.buffered - records)
    const run = this.newRun('dock-telemetry', 1, [key], now, 'seal', 3, 0.8)
    this.emit(run.view.run_id, { type: 'window.sealed', node: 'docks', partition: key, records })
    this.advance(run)
  }

  // ---------------------------------------------------------------- demo controls

  readonly controls: DemoControls = {
    restartDaemon: () => {
      if (this.down) return
      const active = [...this.runs.values()].filter((r) => r.view.state === 'running')
      const inFlight = active.flatMap((r) => r.instances.filter((i) => i.state === 'running'))
      this.emit('daemon', { type: 'daemon.stopped', signal: 'SIGKILL', runs_active: active.length, attempts_in_flight: inFlight.length })
      this.down = true
      setTimeout(() => {
        this.down = false
        this.startedAt = Date.now()
        this.emit('daemon', { type: 'daemon.started', resumed_runs: active.length, re_run: inFlight.length, finished_partitions_untouched: true })
        for (const run of active) {
          for (const inst of run.instances) {
            if (inst.state !== 'running') continue
            inst.attempt++
            inst.last_cause = 'daemon_restart'
            inst.updated_at = iso(Date.now())
            this.emit(run.view.run_id, { type: 'node', node: inst.node, partition: inst.partition, from: 'running', to: 'running', attempt: inst.attempt, cause: 'daemon_restart' }, inst.instance_id)
          }
          this.emit(run.view.run_id, { type: 'run.resumed', partitions_done: run.partitions.filter((p) => this.partitionState(run, p) === 'done').length })
        }
        this.emit('daemon', { type: 'stream.recovered', node: 'docks', records_in_log: this.stream.buffered + this.stream.open.records })
      }, RESTART_MS)
    },
    lateData: () => {
      if (this.down) return
      const s = this.stream
      const w = s.recent[2] ?? s.recent[0]
      if (!w) return
      const late = 23 + (fnv(w.key + w.revision) % 30)
      w.revision++
      w.records += late
      w.blake3 = hex(`window:${w.key}:${w.revision}`)
      s.late += late
      const run = this.newRun('dock-telemetry', 1, [w.key], Date.now(), `rev${w.revision}`, 3, 0.8)
      this.emit(run.view.run_id, { type: 'window.revised', node: 'docks', partition: w.key, revision: w.revision, late })
      this.advance(run)
    },
    reset: () => {
      window.location.reload()
    },
    simulateSecret: (name: string) => {
      this.extras.secrets.add(name)
    },
  }

  // ---------------------------------------------------------------- reads

  private guard(): void {
    if (this.down) throw err('api.unavailable', 'unavailable', 'ogund is not running (killed with SIGKILL); it restarts in a moment', true)
  }

  info(): ShellInfo {
    const platform = /Win/.test(navigator.platform) ? 'windows' : /Mac/.test(navigator.platform) ? 'macos' : /Linux/.test(navigator.platform) ? 'linux' : 'web'
    return {
      backend: 'demo',
      appVersion: __APP_VERSION__,
      platform,
      mobile: false,
      link: 'local',
      profile: 'default',
      connected: !this.down,
      ...(this.down ? { error: err('api.unavailable', 'unavailable', 'ogund is restarting', true) } : {}),
      canWrite: true,
      decideChannel: 'studio',
      canReadSettings: true,
      gateway: false,
      agent: true,
    }
  }

  status(): StatusResult {
    this.guard()
    const active = [...this.runs.values()].filter((r) => r.view.state === 'running' || r.view.state === 'waiting').length
    return {
      daemon_version: '0.1.0-dev',
      started_at: iso(this.startedAt),
      profile: 'default',
      workflows: this.workflows.length,
      runs_active: active,
      approvals_pending: [...this.approvals.values()].filter((a) => a.state === 'pending').length,
      streams: 1,
      last_event_seq: this.seq,
      scheduler_enabled: this.extras.schedulerEnabled,
    }
  }

  workflowsList(): ListWorkflowsResult {
    this.guard()
    return { workflows: this.workflows.map((w) => this.summary(w)) }
  }

  workflowsGet(name: string, version?: number): GetWorkflowResult {
    this.guard()
    const w = this.workflow(name)
    const v = this.version(w, version)
    return { summary: this.summary(w), version: v.version, workflow_hash: v.hash, workflow: clone(v.doc) }
  }

  validate(workflow: unknown): ValidateResult {
    this.guard()
    const diagnostics: Diagnostic[] = []
    const error = (code: string, path: string, message: string): void => {
      diagnostics.push({ severity: 'error', code, path, message })
    }
    const warn = (code: string, path: string, message: string): void => {
      diagnostics.push({ severity: 'warning', code, path, message })
    }
    if (!workflow || typeof workflow !== 'object' || Array.isArray(workflow)) {
      error('ir.not_object', '', 'a workflow document is a JSON object')
      return { ok: false, diagnostics }
    }
    const doc = workflow as Record<string, unknown>
    for (const m of ['workflow', 'version', 'nodes']) if (!(m in doc)) error('ir.missing_member', `/${m}`, `the member ${m} is required`)
    if (doc.ir_version !== 0) error('ir.version', '/ir_version', 'ir_version must be 0')
    const walk = (v: unknown, path: string) => {
      if (typeof v === 'number') {
        if (!Number.isInteger(v)) error('ir.float', path, 'numbers in the IR are integers; scale the value (ADR 0003)')
      } else if (Array.isArray(v)) v.forEach((x, i) => walk(x, `${path}/${i}`))
      else if (v && typeof v === 'object') {
        for (const [k, x] of Object.entries(v as Record<string, unknown>)) {
          if (SECRETISH.test(k) && typeof x === 'string')
            error('ir.secret_inline', `${path}/${k}`, `${k} looks like a secret; use {"secret_ref": "name"} so the value never enters the IR.`)
          walk(x, `${path}/${k}`)
        }
      }
    }
    walk(doc, '')
    const nodes = Array.isArray(doc.nodes) ? (doc.nodes as WorkflowNodeDoc[]) : []
    const ids = new Set(nodes.map((n) => n?.id))
    nodes.forEach((n, i) => {
      if (!n || typeof n !== 'object') return error('ir.node', `/nodes/${i}`, 'a node is an object')
      if (!n.id) error('ir.node_id', `/nodes/${i}/id`, 'every node has an id')
      if (!n.op) error('ir.node_op', `/nodes/${i}/op`, 'every node names its operator as name@major')
      else if (!KNOWN_OPS.has(n.op)) error('ir.operator_unknown', `/nodes/${i}/op`, `${n.op} is not in this daemon's operator set`)
      for (const [j, input] of (n.inputs ?? []).entries()) {
        const from = input.split('.')[0]
        if (!from || !ids.has(from)) error('ir.input_unknown', `/nodes/${i}/inputs/${j}`, `${input} names a node that does not exist`)
      }
      if (n.op === 'python@1' || n.op === 'sql@1') {
        if (!n.code?.blake3) error('ir.code_hash', `/nodes/${i}/code`, 'a code operator is referenced by the BLAKE3 of its code (ADR 0003)')
      }
    })
    if (!doc.budgets && !doc.streams) warn('ir.no_budget', '/budgets', 'no budget: the daemon applies the policy defaults')
    const ok = !diagnostics.some((d) => d.severity === 'error')
    if (!ok) return { ok, diagnostics }
    const key = `${String(doc.workflow)}@${String(doc.version)}`
    const known = DOCS[key] && JSON.stringify(DOCS[key]) === JSON.stringify(doc) ? HASHES[key] : undefined
    return { ok, workflow_hash: known ?? hex('wf:' + JSON.stringify(doc)), diagnostics }
  }

  put(workflow: unknown, idempotencyKey: string): PutWorkflowResult {
    this.guard()
    const check = this.validate(workflow)
    if (!check.ok || !check.workflow_hash) throw err('ir.invalid', 'invalid_argument', 'the document is not a valid workflow', false, { diagnostics: check.diagnostics })
    const doc = workflow as WorkflowDoc
    let w = this.workflows.find((x) => x.name === doc.workflow)
    if (!w) {
      w = { id: 'wf_' + ulid(Date.now(), 'wf:' + doc.workflow), name: doc.workflow, enabled: true, versions: [] }
      this.workflows.push(w)
    }
    const existing = w.versions.find((v) => v.hash === check.workflow_hash)
    if (existing) {
      return { workflow_id: w.id, name: w.name, version: existing.version, workflow_hash: existing.hash, created: false, diagnostics: check.diagnostics, approvals: [] }
    }
    const prev = w.versions[w.versions.length - 1]
    const version = (prev?.version ?? 0) + 1
    const prevCode = new Set((prev?.doc.nodes ?? []).map((n) => n.code?.blake3).filter(Boolean))
    const newCode = doc.nodes.filter((n) => n.code?.blake3 && !prevCode.has(n.code.blake3))
    const v: Version = { version, hash: check.workflow_hash, doc: clone(doc), runnable: newCode.length === 0, storedAt: Date.now() }
    w.versions.push(v)
    const approvals = newCode.map((n) => {
      const a = this.addApproval({
        kind: 'code_change',
        state: 'pending',
        workflow: w!.name,
        workflow_hash: v.hash,
        node: n.id,
        facts: [
          { label: 'operator', value: n.op },
          { label: 'code, new', value: n.code?.blake3 ?? '' },
          ...(n.code?.path ? [{ label: 'path', value: n.code.path }] : []),
        ],
        requested_by: `studio import (${idempotencyKey.slice(0, 18)}…)`,
        created_at: iso(Date.now()),
        expires_at: iso(Date.now() + DAY),
      })
      return { approval_id: a.approval_id, kind: a.kind }
    })
    this.emit('daemon', { type: 'workflow.stored', workflow: w.name, version, approvals: approvals.length })
    return { workflow_id: w.id, name: w.name, version, workflow_hash: v.hash, created: true, diagnostics: check.diagnostics, approvals }
  }

  runsList(params: ListRunsParams): ListRunsResult {
    this.guard()
    let runs = [...this.runs.values()].sort((a, b) => b.createdAt - a.createdAt)
    if (params.workflow) runs = runs.filter((r) => r.view.workflow === params.workflow)
    if (params.before) {
      const cut = this.runs.get(params.before)?.createdAt
      if (cut !== undefined) runs = runs.filter((r) => r.createdAt < cut)
    }
    const limit = Math.max(1, Math.min(params.limit ?? 50, 500))
    const page = runs.slice(0, limit)
    const last = page[page.length - 1]
    return { runs: page.map((r) => clone(r.view)), ...(runs.length > limit && last ? { next: last.view.run_id } : {}) }
  }

  runsGet(runId: string): RunView {
    this.guard()
    const r = this.runs.get(runId)
    if (!r) throw err('run.not_found', 'not_found', `no run ${runId}`)
    return { ...clone(r.view), nodes: clone(r.instances) }
  }

  runsStart(params: StartRunParams): RunView {
    this.guard()
    const seen = this.idempotent.get(params.idempotency_key)
    if (seen) return this.runsGet(seen)
    const w = this.workflow(params.workflow)
    const v = this.version(w, params.version)
    if (params.workflow_hash && params.workflow_hash !== v.hash)
      throw err('workflow.hash_mismatch', 'failed_precondition', `version ${v.version} has hash ${v.hash.slice(0, 8)}…, not the one the request named`)
    if (!v.runnable) {
      const pending = [...this.approvals.values()].find((a) => a.workflow === w.name && a.state === 'pending' && a.workflow_hash === v.hash)
      throw err('workflow.not_runnable', 'failed_precondition', `${w.name} v${v.version} waits for approval${pending ? ' ' + pending.approval_id : ''}`)
    }
    const now = Date.now()
    let keys: string[]
    const sel = params.partitions
    if (sel.kind === 'latest') keys = [granularity(w.name) === 'month' ? monthOf(now - 32 * DAY) : dayOf(now)]
    else if (sel.kind === 'keys') keys = sel.keys
    else {
      keys = keysBetween(w.name, sel.from, sel.to)
      if (!sel.force) {
        const done = this.doneKeys(w.name)
        keys = keys.filter((k) => !done.has(k))
      }
    }
    if (!keys.length) throw err('run.nothing_to_do', 'failed_precondition', 'every partition in the range is already done; force to re-run them')
    const run = this.newRun(w.name, v.version, keys, now, params.idempotency_key, 4, 0.5)
    this.idempotent.set(params.idempotency_key, run.view.run_id)
    this.emit(run.view.run_id, { type: 'run', state: 'running', partitions: keys.length, started_by: 'studio' })
    return this.runsGet(run.view.run_id)
  }

  runsCancel(runId: string): RunView {
    this.guard()
    const r = this.runs.get(runId)
    if (!r) throw err('run.not_found', 'not_found', `no run ${runId}`)
    if (r.view.state === 'succeeded' || r.view.state === 'failed' || r.view.state === 'cancelled')
      throw err('run.finished', 'failed_precondition', `run is already ${r.view.state}`)
    for (const i of r.instances) if (!TERMINAL.has(i.state)) this.transition(r, i, 'cancelled', 'cancelled_by_user')
    r.view.state = 'cancelled'
    this.emit(runId, { type: 'run', state: 'cancelled' })
    return this.runsGet(runId)
  }

  private partitionState(run: Run, key: string): PartitionState {
    const inst = run.instances.filter((i) => i.partition === key)
    if (!inst.length) return 'missing'
    if (inst.every((i) => i.state === 'succeeded')) return 'done'
    if (inst.some((i) => i.state === 'running' || i.state === 'retry_scheduled' || i.state === 'waiting_for_user')) return 'running'
    if (inst.some((i) => i.state === 'failed')) return 'failed'
    return 'missing'
  }

  private doneKeys(name: string): Set<string> {
    const done = new Set<string>()
    for (const r of this.runs.values()) {
      if (r.view.workflow !== name) continue
      for (const p of r.partitions) if (this.partitionState(r, p) === 'done') done.add(p)
    }
    return done
  }

  private keyState(name: string, key: string): PartitionState {
    let best: PartitionState = 'missing'
    for (const r of this.runs.values()) {
      if (r.view.workflow !== name || !r.partitions.includes(key)) continue
      const s = this.partitionState(r, key)
      if (s === 'done') return 'done'
      if (s === 'running') best = 'running'
      else if (s === 'failed' && best === 'missing') best = 'failed'
    }
    return best
  }

  partitionsStatus(params: PartitionStatusParams): PartitionStatusResult {
    this.guard()
    const w = this.workflow(params.workflow)
    const v = this.version(w, params.version)
    const now = Date.now()
    let ranges: PartitionRange[]
    if (w.name === 'dock-telemetry') {
      const s = this.stream
      ranges = [
        { from: windowKey(now - (s.sealed + 1) * MIN), to: s.recent[0]?.key ?? windowKey(now - MIN), count: s.sealed, state: 'done' },
        { from: s.open.key, to: s.open.key, count: 1, state: 'running' },
      ]
    } else {
      const keys =
        w.name === 'trips-backfill'
          ? monthKeys('2019-01', monthOf(now - 32 * DAY))
          : w.name === 'district-report'
            ? monthKeys('2026-01', monthOf(now - 32 * DAY))
            : w.name === 'ridership-forecast'
              ? [...this.runs.values()].filter((r) => r.view.workflow === w.name).flatMap((r) => r.partitions).sort()
              : dayKeys(dayOf(now - 29 * DAY), dayOf(now))
      const scoped = keys.filter((k) => (!params.from || k >= params.from) && (!params.to || k <= params.to))
      ranges = compress(scoped, (k) => (w.name === 'district-report' && k < '2026-09' ? 'done' : this.keyState(w.name, k)))
    }
    return { workflow_hash: v.hash, ranges }
  }

  backfillsPlan(workflow: string, from: string, to: string): PlanBackfillResult {
    this.guard()
    const w = this.workflow(workflow)
    const v = this.version(w)
    const keys = keysBetween(w.name, from, to)
    if (!keys.length) throw err('api.invalid_params', 'invalid_argument', `no ${granularity(w.name)} keys between ${from} and ${to}`)
    const done = this.doneKeys(w.name)
    const ranges = compress(keys, (k) => (done.has(k) ? 'done' : 'missing'))
    const upToDate = keys.filter((k) => done.has(k)).length
    return { workflow_hash: v.hash, to_run: keys.length - upToDate, up_to_date: upToDate, ranges }
  }

  approvalsList(state?: ApprovalState): ListApprovalsResult {
    this.guard()
    const all = [...this.approvals.values()].filter((a) => !state || a.state === state)
    all.sort((a, b) => b.created_at.localeCompare(a.created_at))
    return { approvals: clone(all) }
  }

  approvalsGet(id: string): ApprovalView {
    this.guard()
    const a = this.approvals.get(id)
    if (!a) throw err('approval.not_found', 'not_found', `no approval ${id}`)
    return clone(a)
  }

  prepareDecision(id: string, decision: Decision): PreparedDecision {
    this.guard()
    const a = this.approvals.get(id)
    if (!a) throw err('approval.not_found', 'not_found', `no approval ${id}`)
    if (a.state !== 'pending') throw err('approval.already_decided', 'failed_precondition', `this request is ${a.state}`)
    const snapshot = clone(a)
    let used = false
    return {
      decision,
      approval: snapshot,
      armMs: 1500,
      confirm: async () => {
        if (used) throw err('api.invalid_params', 'invalid_argument', 'this review was already used')
        used = true
        await sleep(350)
        this.guard()
        const live = this.approvals.get(id)
        if (!live || live.state !== 'pending') throw err('approval.already_decided', 'failed_precondition', 'the request was decided elsewhere')
        if (live.request_hash !== snapshot.request_hash) throw err('approval.request_changed', 'failed_precondition', 'the facts changed since the review; look again')
        live.state = decision === 'approve' ? 'approved' : 'denied'
        live.decided_at = iso(Date.now())
        live.decided_via = 'studio'
        live.decided_by = 'operator'
        this.emit(live.run_id ?? 'daemon', { type: 'approval', kind: live.kind, state: live.state, workflow: live.workflow, node: live.node ?? '' })
        this.afterDecision(live)
        return clone(live)
      },
      cancel: () => {
        used = true
      },
    }
  }

  private afterDecision(a: ApprovalView): void {
    const w = this.workflows.find((x) => x.name === a.workflow)
    if (a.kind === 'code_change' && w) {
      const v = w.versions.find((x) => x.hash === a.workflow_hash)
      if (v && a.state === 'approved') {
        v.runnable = true
        setTimeout(() => {
          if (this.down) return
          const run = this.newRun(w.name, v.version, [dayOf(Date.now())], Date.now(), 'after-approval', 3, 0.55)
          this.emit(run.view.run_id, { type: 'run', state: 'running', partitions: 1, started_by: 'approval ' + a.approval_id })
          this.advance(run)
        }, 900)
      }
    }
    if (a.kind === 'publish' && a.run_id) {
      const run = this.runs.get(a.run_id)
      const inst = run && a.node ? this.instance(run, a.node, run.partitions[0] ?? '') : undefined
      if (run && inst) {
        if (a.state === 'approved') {
          run.view.state = 'running'
          inst.attempt = 1
          this.transition(run, inst, 'running')
        } else {
          this.transition(run, inst, 'cancelled', 'denied_by_user')
          run.view.state = 'cancelled'
          this.emit(run.view.run_id, { type: 'run', state: 'cancelled' })
        }
      }
    }
  }

  // ---------------------------------------------------------------- artifacts and lineage (derived from runs)

  private artifactsOf(run: Run): ArtifactView[] {
    const cached = this.artifactsByRun.get(run.view.run_id)
    if (cached) return cached
    const out: ArtifactView[] = []
    for (const inst of run.instances) {
      if (inst.state !== 'succeeded') continue
      const node = run.doc.nodes.find((n) => n.id === inst.node)
      if (!node) continue
      for (const [output, media, bytes, content] of this.outputsOf(run, node, inst)) {
        const blake3 = hex(`art:${run.view.run_id}:${inst.node}:${inst.partition}:${output}`)
        out.push({
          artifact_id: 'art_' + ulid(new Date(inst.updated_at).getTime(), 'art:' + blake3),
          blake3,
          size_bytes: content ? new TextEncoder().encode(content).length : bytes,
          media_type: media,
          run_id: run.view.run_id,
          node: inst.node,
          partition: inst.partition,
          output,
          committed_at: inst.updated_at,
        })
        if (content) this.artifactContent.set(blake3, content)
      }
    }
    if (run.view.state === 'succeeded' || run.view.state === 'failed' || run.view.state === 'cancelled') this.artifactsByRun.set(run.view.run_id, out)
    return out
  }

  /** `[output, media type, size, inline content]` for one node instance. */
  private outputsOf(run: Run, node: WorkflowNodeDoc, inst: NodeView): [string, string, number, string | undefined][] {
    const seed = `${run.view.run_id}:${node.id}:${inst.partition}`
    const next = rng(seed)
    const cfg = (node.config ?? {}) as Record<string, unknown>
    switch (node.op) {
      case 'duckdb@1':
        if (typeof cfg.write === 'string') {
          if (run.view.workflow === 'rebalance-plan' && node.id === 'shortlist')
            return [
              ['artifacts', 'application/vnd.apache.parquet', 11_000 + (next() % 4000), undefined],
              ['csv', 'text/csv', 0, this.shortlistCsv(seed)],
            ]
          return [['artifacts', 'application/vnd.apache.parquet', 40_000 + (next() % 300_000), undefined]]
        }
        return [['tables', 'application/vnd.apache.parquet', 1_000_000 + (next() % 1_500_000), undefined]]
      case 'python@1':
        return [['rows', 'application/x-ndjson', 0, node.id === 'estimate' ? this.estimateRows(seed) : this.forecastRows(seed)]]
      case 'sql@1':
        return [['tables', 'text/csv', 0, node.id === 'figures' ? this.districtFiguresCsv(seed) : this.tripsDailyCsv(seed, inst.partition)]]
      case 'crawl@1':
        return [['urls', 'text/plain', 0, this.archiveUrls(inst.partition)]]
      case 'acquire@1':
        return [['artifacts', 'application/gzip', 2_000_000 + (next() % 9_000_000), undefined]]
      case 'pandoc@1':
        return [
          ['artifacts', 'application/pdf', 180_000 + (next() % 90_000), undefined],
          ['artifacts.es', 'application/pdf', 210_000 + (next() % 90_000), undefined],
        ]
      case 'file.copy@1':
        return [['artifacts', 'application/pdf', 180_000 + (next() % 90_000), undefined]]
      case 'filter@1':
        return [['artifacts', 'application/x-ndjson', 0, this.dockRows(seed, inst.partition)]]
      case 'window.rollup@1':
        return [['artifacts', 'application/x-ndjson', 3_000 + (next() % 2000), undefined]]
      default:
        return [['artifacts', 'application/octet-stream', 1000 + (next() % 5000), undefined]]
    }
  }

  private stations(seed: string, n: number): { id: string; area: string; street: string; capacity: number }[] {
    const next = rng(seed)
    const streets = ['Main Street', 'Canal Road', 'Station Plaza', 'Library Square', 'Park Gate', 'Pier 3', 'Campus North', 'Mill Lane']
    const out = []
    for (let i = 0; i < n; i++) {
      const area = SITES[i % SITES.length] ?? SITES[0]
      out.push({ id: `ST-${area[0]}-${pad2(((next() % 90) + 10) % 100)}${pad2(next() % 100)}`, area: area[1], street: streets[next() % streets.length] ?? 'Main Street', capacity: 14 + 2 * (next() % 8) })
    }
    return out
  }

  /** What `rebalance.py` writes: the empty and full risk of each station over the last thirty days. */
  private estimateRows(seed: string): string {
    const next = rng(seed)
    return this.stations(seed, 24)
      .map((s) => {
        const empty = next() % 900
        const full = next() % 700
        return JSON.stringify({ station_id: s.id, empty_risk_pct_x10: empty, empty_worst_hour: 7 + (next() % 3), full_risk_pct_x10: full, full_worst_hour: 17 + (next() % 3) })
      })
      .join('\n') + '\n'
  }

  /** The shortlist: stations whose risk is over the threshold, earliest expected first. */
  private shortlistCsv(seed: string): string {
    const next = rng(seed + ':shortlist')
    const rows = this.stations(seed, 24)
      .map((s) => {
        const risk = 700 + (next() % 280)
        const hours = 1 + (next() % 20)
        const by = new Date(Date.now() + hours * HOUR)
        const when = `${dayOf(by.getTime())} ${pad2(by.getUTCHours())}:00`
        const bikes = next() % 3
        return { when, row: `${s.id},${s.area},${s.street},${bikes},${s.capacity},${(risk / 10).toFixed(1)},${when}` }
      })
      .sort((a, b) => a.when.localeCompare(b.when))
      .slice(0, 14)
      .map((r) => r.row)
    return 'station_id,area,street,bikes,capacity,empty_risk_pct,expected_by\n' + rows.join('\n') + '\n'
  }

  private forecastRows(seed: string): string {
    const next = rng(seed)
    return this.stations(seed, 8)
      .map((s) => JSON.stringify({ station_id: s.id, week_from: dayOf(Date.now() + 7 * DAY), trips_last_year: 300 + (next() % 900), weather_scale_x100: 82 + (next() % 36), trips_forecast: 280 + (next() % 1000) }))
      .join('\n') + '\n'
  }

  private districtFiguresCsv(seed: string): string {
    const next = rng(seed)
    const rows: string[] = []
    for (const s of this.stations(seed, 10)) {
      const hours = 480 + (next() % 24)
      const empty = next() % 40
      const full = next() % 30
      rows.push(`${s.id},${s.area},${s.capacity},${hours},${(4 + (next() % 60) / 10).toFixed(1)},${(s.capacity - 4 - (next() % 40) / 10).toFixed(1)},${(1 - (empty + full) / hours).toFixed(2)},${empty},${full}`)
    }
    return 'station_id,district,docks,hours_measured,avg_bikes,avg_docks_free,availability,hours_empty,hours_full\n' + rows.join('\n') + '\n'
  }

  private tripsDailyCsv(seed: string, partition: string): string {
    const next = rng(seed)
    const rows: string[] = []
    const ids = ['ST-RIV-1042', 'ST-HAR-2217', 'ST-OLD-3375', 'ST-UNI-4120', 'ST-MKT-5588']
    for (let day = 1; day <= 10; day++) {
      for (const id of ids.slice(0, 3)) {
        const trips = 24 + (next() % 160)
        rows.push(`${id},${partition}-${pad2(day)},${trips},${(8 + (next() % 140) / 10).toFixed(1)},${(0.3 + (next() % 55) / 100).toFixed(2)}`)
      }
    }
    return 'station_id,day,trips,avg_duration_min,member_share\n' + rows.join('\n') + '\n'
  }

  private archiveUrls(partition: string): string {
    const [y, m] = partition.split('-')
    return [1042, 2217, 3375, 4120, 5588, 6034, 7109, 7120, 8861]
      .map((id) => `https://open-trips-archive.s3.amazonaws.com/trips/station=${id}/year=${y}/month=${m}/`)
      .join('\n') + '\n'
  }

  private dockRows(seed: string, partition: string): string {
    const next = rng(seed)
    const base = new Date(partition.slice(1) + ':00.000Z').getTime()
    const lines: string[] = []
    for (const s of this.stations(seed, 8)) {
      for (let k = 0; k < 6; k++) {
        const bikes = next() % (s.capacity + 1)
        lines.push(JSON.stringify({ station_id: s.id, seq: 410_000 + (next() % 9000), ts_ms: base + k * 10_000 + (next() % 900), bikes, docks_free: s.capacity - bikes, battery_pct: 40 + (next() % 60), fault: next() % 97 === 0 }))
      }
    }
    return lines.join('\n') + '\n'
  }

  artifactsList(runId: string): ListArtifactsResult {
    this.guard()
    const r = this.runs.get(runId)
    if (!r) throw err('run.not_found', 'not_found', `no run ${runId}`)
    return { artifacts: clone(this.artifactsOf(r)) }
  }

  artifactsGet(blake3: string, inlineMax: number): GetArtifactResult {
    this.guard()
    let found: ArtifactView | undefined
    for (const r of this.runs.values()) {
      found = this.artifactsOf(r).find((a) => a.blake3 === blake3)
      if (found) break
    }
    if (!found) throw err('artifact.not_found', 'not_found', `no artifact ${blake3.slice(0, 12)}…`)
    const content = this.artifactContent.get(blake3)
    const inline = content && found.size_bytes <= inlineMax ? btoa(unescape(encodeURIComponent(content))) : undefined
    return { artifact: clone(found), ...(inline ? { content_base64: inline } : {}) }
  }

  lineageGet(q: { run_id?: string; blake3?: string }): LineageResult {
    this.guard()
    let run: Run | undefined
    let only: ArtifactView | undefined
    if (q.run_id) run = this.runs.get(q.run_id)
    else if (q.blake3) {
      for (const r of this.runs.values()) {
        only = this.artifactsOf(r).find((a) => a.blake3 === q.blake3)
        if (only) {
          run = r
          break
        }
      }
    }
    if (!run) return { events: [] }
    const arts = this.artifactsOf(run)
    const ds = (a: ArtifactView) => ({ namespace: 'ogun://default/artifacts', name: `${a.node}.${a.output}@${a.blake3.slice(0, 12)}` })
    const events: Record<string, unknown>[] = []
    for (const inst of run.instances) {
      if (inst.state === 'pending' || inst.state === 'skipped') continue
      if (only && inst.node !== only.node && !producers(run.doc, only.node).includes(inst.node)) continue
      const inputs = producers(run.doc, inst.node).flatMap((p) => arts.filter((a) => a.node === p && a.partition === inst.partition).map(ds))
      const outputs = arts.filter((a) => a.node === inst.node && a.partition === inst.partition).map(ds)
      const end = new Date(inst.updated_at).getTime()
      const base = {
        producer: 'https://github.com/ArefinAlter/ogun/tree/main/crates/ogun-lineage',
        schemaURL: 'https://openlineage.io/spec/2-0-2/OpenLineage.json#/$defs/RunEvent',
        job: { namespace: 'ogun://default', name: `${run.view.workflow}.${inst.node}` },
        run: { runId: hex('ol:' + inst.instance_id, 32).replace(/^(.{8})(.{4})(.{4})(.{4})(.{12})$/, '$1-$2-$3-$4-$5'), facets: { ogun: { run_id: run.view.run_id, instance_id: inst.instance_id, partition: inst.partition, attempt: inst.attempt } } },
      }
      events.push({ ...base, eventType: 'START', eventTime: iso(end - 20 * SEC), inputs, outputs: [] })
      if (inst.state === 'succeeded') events.push({ ...base, eventType: 'COMPLETE', eventTime: iso(end), inputs, outputs })
      else if (inst.state === 'failed') events.push({ ...base, eventType: 'FAIL', eventTime: iso(end), inputs, outputs: [], run: { ...base.run, facets: { ...base.run.facets, errorMessage: { message: inst.last_cause ?? 'failed', programmingLanguage: 'operator' } } } })
      else events.push({ ...base, eventType: 'RUNNING', eventTime: iso(end), inputs, outputs: [] })
    }
    return { events }
  }

  // ---------------------------------------------------------------- streams, edges, settings

  streamView(): StreamView {
    const s = this.stream
    return {
      workflow: 'dock-telemetry',
      node: 'docks',
      op: 'stream.mqtt@1',
      buffered: s.buffered,
      dropped: s.dropped,
      watermark_ms: s.watermarkMs,
      windows_sealed: s.sealed,
      windows_open: 1,
      open_windows: [{ key: s.open.key, records: s.open.records, closes_at_ms: s.open.closesAt }],
      sealed_windows: clone(s.recent),
      late: s.late,
      invalid: s.invalid,
    }
  }

  llmLadder(): LadderResult {
    this.guard()
    return { available: true, ladder: { version: 1, providers: this.extras.ladderGet() }, spend: this.extras.ladderSpend() }
  }

  subscribe(opts: { after?: number; runId?: string }, onEvent: (e: EventView) => void): EventSubscription {
    const l: Listener = { ...(opts.runId ? { runId: opts.runId } : {}), onEvent }
    this.listeners.add(l)
    return {
      close: () => {
        this.listeners.delete(l)
      },
    }
  }
}

let shared: Demo | null = null

function demo(): Demo {
  shared ??= new Demo()
  return shared
}

/** A little latency, so loading states are visible and the pages behave as they would on a daemon. */
async function call<T>(f: () => T): Promise<T> {
  await sleep(30 + Math.floor(Math.random() * 50))
  try {
    return f()
  } catch (e) {
    throw asOgunError(e)
  }
}

async function callAsync<T>(f: () => Promise<T>): Promise<T> {
  try {
    return await f()
  } catch (e) {
    throw asOgunError(e)
  }
}

export function demoBackend(): Backend {
  const d = demo()
  const backend: Backend = {
    kind: 'demo',
    demo: d.controls,
    agent: createScriptedAgent(() => backend),
    info: () => call(() => d.info()),
    status: () => call(() => d.status()),
    workflowsList: () => call(() => d.workflowsList()),
    workflowsGet: (name, version) => call(() => d.workflowsGet(name, version)),
    workflowsValidate: (workflow) => call(() => d.validate(workflow)),
    workflowsPut: (workflow, key) => call(() => d.put(workflow, key)),
    runsList: (params) => call(() => d.runsList(params)),
    runsGet: (id) => call(() => d.runsGet(id)),
    runsStart: (params) => call(() => d.runsStart(params)),
    runsCancel: (id) => call(() => d.runsCancel(id)),
    partitionsStatus: (params) => call(() => d.partitionsStatus(params)),
    backfillsPlan: (workflow, from, to) => call(() => d.backfillsPlan(workflow, from, to)),
    subscribe: (opts, onEvent) => d.subscribe(opts, onEvent),
    approvalsList: (state) => call(() => d.approvalsList(state)),
    approvalsGet: (id) => call(() => d.approvalsGet(id)),
    prepareDecision: (id, decision) => call(() => d.prepareDecision(id, decision)),
    artifactsList: (runId) => call(() => d.artifactsList(runId)),
    artifactsGet: (blake3, max) => call(() => d.artifactsGet(blake3, max)),
    lineageGet: (q) => call(() => d.lineageGet(q)),
    streamsList: () => call(() => ({ streams: [d.streamView()] })),
    streamsStatus: () => call(() => d.streamView()),
    edgesList: () => call(() => ({ edges: clone(d.edges) })),
    credentialsList: () => call(() => ({ credentials: clone(d.credentials) })),
    secretsList: () => call(() => ({ names: d.extras.secretNames() })),
    llmLadder: () => call(() => d.llmLadder()),
    connections: {
      list: () => call(() => ({ connections: d.extras.connectionsList() })),
      put: (input) => call(() => d.extras.connectionPut(input)),
      test: (name) => callAsync(() => d.extras.connectionTest(name)),
      remove: (name) => call(() => d.extras.connectionRemove(name)),
    },
    schedules: () => call(() => ({ schedules: d.extras.schedules() })),
    setWorkflowEnabled: (name, enabled) => call(() => d.extras.setWorkflowEnabled(name, enabled)),
    addSchedule: (workflow, cron, key) => call(() => d.extras.addSchedule(workflow, cron, key)),
    runtime: {
      get: (workflow) => call(() => d.extras.runtimeGet(workflow)),
      set: (workflow, patch) => call(() => d.extras.runtimeSet(workflow, patch)),
      reset: (workflow, field) => call(() => d.extras.runtimeReset(workflow, field)),
    },
    environments: () => call(() => d.extras.environments(d.edges, '0.1.0-dev', 'default')),
    setLadder: (providers) => call(() => void d.extras.ladderPut(providers)),
    setSchedulerEnabled: (enabled) =>
      call(() => {
        d.extras.schedulerEnabled = enabled
      }),
    sources: {
      get: () => call(() => d.extras.sourcesGet()),
      setRepo: (id, patch) => call(() => d.extras.sourcesSetRepo(id, patch)),
      sync: (id) => call(() => d.extras.sourcesSync(id)),
      setProvider: (id, enabled) => call(() => d.extras.sourcesSetProvider(id, enabled)),
      setAutoPull: (on) => call(() => d.extras.sourcesAutoPull(on)),
    },
    evidence: {
      get: () => call(() => d.extras.evidenceGet()),
      set: (patch) => call(() => d.extras.evidenceSet(patch)),
    },
    storage: {
      info: () => call(() => d.extras.storageInfo()),
      setRetention: (days) => call(() => (d.extras.storageSetRules({ retention_days: days }), d.extras.storageInfo())),
      clearCaches: () => call(() => d.extras.storageClearCaches()),
      rules: () => call(() => d.extras.storageRules()),
      setRules: (patch) => call(() => d.extras.storageSetRules(patch)),
      cleanupPlan: () => call(() => d.extras.storageCleanupPlan()),
      cleanup: () => call(() => d.extras.storageCleanup()),
    },
    archive: {
      list: () => call(() => ({ items: d.extras.archiveList() })),
      restore: (id) => call(() => d.extras.archiveRestore(id)),
    },
  }
  return backend
}

export type { ApprovalKind }
