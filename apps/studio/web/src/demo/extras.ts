// The rest of the scripted daemon: the state behind Settings. Secrets (names only), data connections, run
// defaults with per-workflow overrides, environments and bridges, workflow sources, evidence, storage rules,
// the archive and the provider ladder. Everything here is invented except where it is derived from
// the example workflows (a workflow's budgets become that workflow's overrides, its triggers become schedules).

import type {
  ArchiveItem,
  CleanupPlan,
  ConnectionInput,
  ConnectionTestResult,
  ConnectionValue,
  ConnectionView,
  EnvironmentsResult,
  EvidenceSettings,
  LadderProvider,
  PutWorkflowResult,
  RunState,
  RuntimeSettings,
  RuntimeSettingsView,
  ScheduleView,
  SourcesResult,
  StorageInfo,
  StorageRules,
  WorkflowDoc,
} from '../api/types'
import { parseCron } from '../lib/cron'
import type { Run, Workflow } from './index'
import { DAY, HOUR, MIN, clone, err, hex, iso, ulid } from './util'

/** What the extras need from the main scripted daemon. */
export interface Host {
  workflows: () => Workflow[]
  runs: () => Map<string, Run>
  put: (workflow: unknown, key: string) => PutWorkflowResult
  guard: () => void
  notify: (kind: Record<string, unknown>) => void
}

const DEFAULT_RUNTIME: RuntimeSettings = {
  concurrency: 4,
  retries: 3,
  wall_minutes: 60,
  bytes_gib: 2,
  approval_expiry_hours: 24,
  model_access: 'off',
  auto_pause_after_failures: 3,
  auto_archive_days: 60,
  keep_attention_runs: true,
  resume_on_start: true,
}

const GIB = 1024 ** 3

export class DemoExtras {
  readonly secrets = new Set<string>(['city_mqtt', 'archive_key', 'open_meteo', 'report_mailer', 'anthropic_api_key', 'datadog_api_key', 'dingtalk_webhook'])
  schedulerEnabled = true
  private connections: ConnectionView[]
  private defaults: RuntimeSettings = { ...DEFAULT_RUNTIME }
  private overrides = new Map<string, Partial<RuntimeSettings>>()
  private evidence: EvidenceSettings = { enabled: true, format: 'json', include_lineage: true, include_previews: false }
  private rules: StorageRules = {
    store_location: '/var/lib/ogun/profiles/default/artifacts',
    limit_gib: 100,
    retention_days: 90,
    delete_failed_after_days: 30,
    delete_cancelled_after_days: 7,
    delete_partials_after_days: 2,
    delete_logs_after_days: 14,
    clear_caches_after_days: 30,
    keep_lineage_referenced: true,
  }
  private store = {
    artifacts: 18_742,
    bytes: 38.6 * GIB,
    runs: 1_113,
    caches: 6.2 * GIB,
    byWorkflow: [
      { workflow: 'trips-backfill', artifacts: 9_840, bytes: 24.1 * GIB },
      { workflow: 'dock-telemetry', artifacts: 7_212, bytes: 9.8 * GIB },
      { workflow: 'rebalance-plan', artifacts: 1_054, bytes: 3.1 * GIB },
      { workflow: 'district-report', artifacts: 412, bytes: 1.0 * GIB },
      { workflow: 'ridership-forecast', artifacts: 224, bytes: 0.6 * GIB },
    ],
  }
  private archived: ArchiveItem[]
  private sources: SourcesResult
  private ladder: LadderProvider[]

  constructor(private readonly host: Host) {
    const now = Date.now()
    this.connections = [
      {
        connection_id: 'cnx_' + ulid(now - 12 * DAY, 'cnx:s3'),
        name: 'trips-archive',
        connector: 'aws-s3',
        auth: 'anonymous',
        settings: { region: 'us-east-1', bucket: 'open-trips-archive', prefix: 'trips/', path_style: false },
        secret_refs: {},
        state: 'healthy',
        last_test_at: iso(now - 2 * HOUR),
        last_test_ms: 412,
        created_at: iso(now - 12 * DAY),
        used_by: ['trips-backfill'],
      },
      {
        connection_id: 'cnx_' + ulid(now - 5 * DAY, 'cnx:bq'),
        name: 'analytics-bigquery',
        connector: 'bigquery',
        auth: 'service_account',
        settings: { project_id: 'city-analytics', location: 'europe-west4', dataset: 'bikeshare' },
        secret_refs: { service_account_json: 'analytics_bq_key' },
        state: 'failing',
        last_test_at: iso(now - 40 * MIN),
        last_test_ms: 188,
        last_error: 'secret analytics_bq_key is not stored',
        created_at: iso(now - 5 * DAY),
        used_by: [],
      },
      {
        connection_id: 'cnx_' + ulid(now - 9 * DAY, 'cnx:dd'),
        name: 'ops-datadog',
        connector: 'datadog',
        auth: 'api_key',
        settings: { site: 'datadoghq.com', service: 'ogun', env: 'production' },
        secret_refs: { api_key: 'datadog_api_key' },
        state: 'healthy',
        last_test_at: iso(now - 26 * MIN),
        last_test_ms: 326,
        created_at: iso(now - 9 * DAY),
        used_by: [],
      },
      {
        connection_id: 'cnx_' + ulid(now - 3 * DAY, 'cnx:dt'),
        name: 'ops-dingtalk',
        connector: 'dingtalk',
        auth: 'webhook',
        settings: {},
        secret_refs: { webhook_url: 'dingtalk_webhook' },
        state: 'untested',
        created_at: iso(now - 3 * DAY),
        used_by: [],
      },
    ]
    this.archived = [
      { id: 'arc_' + hex('a1', 8), kind: 'workflow_version', name: 'noaa-daily v1', detail: 'Daily NOAA CSV files since 2020, one partition per day', archived_at: iso(now - 20 * DAY), reason: 'Set aside' },
      { id: 'arc_' + hex('a2', 8), kind: 'workflow_version', name: 'climate-papers v18', detail: 'Weekly paper crawl with an OCR route', archived_at: iso(now - 31 * DAY), reason: 'Superseded by v19' },
      { id: 'arc_' + hex('a3', 8), kind: 'run', name: 'orun_01M3B2…FQ7', detail: 'rebalance-plan, 1 partition, succeeded', archived_at: iso(now - 9 * DAY), reason: 'Auto-archived after 60 days' },
      { id: 'arc_' + hex('a4', 8), kind: 'run', name: 'orun_01M3A9…K2D', detail: 'trips-backfill, 93 partitions, succeeded', archived_at: iso(now - 14 * DAY), reason: 'Auto-archived after 60 days' },
    ]
    this.sources = {
      auto_pull: false,
      repos: [
        { id: 'src_examples', name: 'Pipeline repository', kind: 'git', location: 'https://git.example.org/data/pipelines', branch: 'main', watch: false, import_mode: 'store_validate', workflows: 6, last_sync_at: iso(now - 3 * HOUR), last_result: '6 workflows up to date' },
        { id: 'src_local', name: 'Local workflows', kind: 'folder', location: '~/ogun/workflows', watch: true, import_mode: 'store', workflows: 6, last_sync_at: iso(now - 25 * MIN), last_result: 'stored rebalance-plan v2; 1 approval needed' },
      ],
      providers: [
        { id: 'git', name: 'Git', region: 'global', state: 'available', detail: 'git version 2.51.2', enabled: true },
        { id: 'github', name: 'GitHub', region: 'global', state: 'available', detail: 'gh version 2.96.0, authenticated', enabled: true },
        { id: 'gitlab', name: 'GitLab', region: 'global', state: 'not_installed', detail: 'Not available on this machine. Install the GitLab command-line tool (glab).', enabled: false },
        { id: 'azure-devops', name: 'Azure DevOps', region: 'global', state: 'not_installed', detail: 'Not available on this machine. Install the Azure command-line tools (az), then add the azure-devops extension.', enabled: false },
        { id: 'bitbucket', name: 'Bitbucket', region: 'global', state: 'not_authenticated', detail: 'Available. Add a Bitbucket token with ogun secrets set.', enabled: false },
        { id: 'gitee', name: 'Gitee', region: 'cn', state: 'not_installed', detail: 'Coming soon. Folders and plain git remotes work today.', enabled: false },
        { id: 'coding', name: 'Coding (Tencent Cloud)', region: 'cn', state: 'not_installed', detail: 'Coming soon. Folders and plain git remotes work today.', enabled: false },
      ],
    }
    this.ladder = [
      { name: 'anthropic-opus', kind: 'anthropic', enabled: true, base_url: 'https://api.anthropic.com', model: 'claude-opus-5-5', api_key_secret: 'anthropic_api_key', input_per_mtok_usd_micro: 0, output_per_mtok_usd_micro: 0, max_output_tokens: 16000, timeout_ms: 300000, capabilities: ['chat', 'json_schema'], allow_insecure_http: false },
      { name: 'anthropic-sonnet', kind: 'anthropic', enabled: true, base_url: 'https://api.anthropic.com', model: 'claude-sonnet-5-5', api_key_secret: 'anthropic_api_key', input_per_mtok_usd_micro: 2000000, output_per_mtok_usd_micro: 10000000, max_output_tokens: 16000, timeout_ms: 300000, capabilities: ['chat', 'json_schema'], allow_insecure_http: false },
      { name: 'ollama', kind: 'ollama', enabled: true, base_url: 'http://127.0.0.1:11434', model: 'qwen3:8b', input_per_mtok_usd_micro: 0, output_per_mtok_usd_micro: 0, max_output_tokens: 8192, timeout_ms: 120000, capabilities: ['chat', 'json_schema', 'embeddings'], allow_insecure_http: false },
    ]
  }

  // ---------------------------------------------------------------- secrets and connections

  secretNames(): string[] {
    return [...this.secrets].sort()
  }

  connectionsList(): ConnectionView[] {
    this.host.guard()
    return clone(this.connections)
  }

  connectionPut(input: ConnectionInput): ConnectionView {
    this.host.guard()
    const existing = this.connections.find((c) => c.name === input.name)
    const view: ConnectionView = {
      connection_id: existing?.connection_id ?? 'cnx_' + ulid(Date.now(), 'cnx:' + input.name),
      name: input.name,
      connector: input.connector,
      auth: input.auth,
      settings: input.settings,
      secret_refs: input.secret_refs,
      state: 'untested',
      created_at: existing?.created_at ?? iso(Date.now()),
      used_by: existing?.used_by ?? [],
    }
    if (existing) this.connections[this.connections.indexOf(existing)] = view
    else this.connections.push(view)
    this.host.notify({ type: 'connection.saved', name: input.name, connector: input.connector })
    return clone(view)
  }

  connectionRemove(name: string): void {
    this.host.guard()
    const i = this.connections.findIndex((c) => c.name === name)
    if (i < 0) throw err('connection.not_found', 'not_found', `no connection named ${name}`)
    this.connections.splice(i, 1)
  }

  async connectionTest(name: string): Promise<ConnectionTestResult> {
    this.host.guard()
    const c = this.connections.find((x) => x.name === name)
    if (!c) throw err('connection.not_found', 'not_found', `no connection named ${name}`)
    const { CATALOG } = await import('../connectors/catalog.generated')
    const m = CATALOG.find((x) => x.id === c.connector)
    const checks: ConnectionTestResult['checks'] = []
    if (!m) {
      checks.push({ name: 'connector', ok: false, detail: `no connector ${c.connector} in the catalog` })
    } else {
      const missing = m.settings.filter((s) => s.required && (c.settings[s.name] === undefined || c.settings[s.name] === ''))
      checks.push({ name: 'settings', ok: missing.length === 0, detail: missing.length ? `${missing.map((s) => s.label).join(', ')} not set` : `${m.settings.length} settings valid` })
      const auth = m.auth.find((a) => a.id === c.auth)
      const secretFields = (auth?.fields ?? []).filter((f) => f.secret && (f.required || c.secret_refs[f.name]))
      const unstored = secretFields.filter((f) => !c.secret_refs[f.name] || !this.secrets.has(c.secret_refs[f.name] as string))
      checks.push({
        name: 'secrets',
        ok: unstored.length === 0,
        detail: secretFields.length === 0 ? 'none needed' : unstored.length ? unstored.map((f) => `${c.secret_refs[f.name] ?? f.name} is not stored; run ogun secrets set ${c.secret_refs[f.name] ?? f.name}`).join('; ') : `${secretFields.length} stored, values not read`,
      })
      const hosts = m.egress.domains.map((d) => {
        let done = true
        const host = d.replace(/\{([a-z_][a-z0-9_]*)\}/g, (w, k: string) => {
          const v = c.settings[k] ?? m.settings.find((s) => s.name === k)?.default
          if (v === undefined || v === '') {
            done = false
            return w
          }
          return String(v)
        })
        return { host, done }
      })
      const incomplete = hosts.filter((h) => !h.done)
      checks.push({ name: 'hosts', ok: incomplete.length === 0, detail: hosts.length === 0 ? 'none: nothing leaves this machine' : incomplete.length ? `${incomplete.map((h) => h.host).join(', ')} still has a placeholder` : `${hosts.length} to allow: ${hosts.map((h) => h.host).slice(0, 3).join(', ')}${hosts.length > 3 ? ', …' : ''}` })
      const ok = checks.every((x) => x.ok)
      if (m.maturity === 'contract') {
        checks.push({ name: 'reach', ok: true, detail: `not attempted: no operator for ${m.name} exists yet, so only the settings, secrets and hosts are checked` })
      } else if (ok) {
        checks.push({ name: 'authenticate', ok: true, detail: `${auth?.label ?? 'no credentials'}: accepted` })
        checks.push({ name: 'read', ok: true, detail: 'listed one object' })
      } else checks.push({ name: 'authenticate', ok: false, detail: 'skipped: fix the checks above first' })
    }
    const ok = checks.every((x) => x.ok)
    const elapsed = 180 + (parseInt(hex('t:' + name + checks.length, 4), 16) % 620)
    c.state = ok ? 'healthy' : 'failing'
    c.last_test_at = iso(Date.now())
    c.last_test_ms = elapsed
    if (ok) delete c.last_error
    else c.last_error = checks.find((x) => !x.ok)?.detail ?? 'failed'
    await new Promise((r) => setTimeout(r, Math.min(elapsed, 700)))
    return { ok, elapsed_ms: elapsed, simulated: true, checks }
  }

  // ---------------------------------------------------------------- schedules

  schedules(): ScheduleView[] {
    this.host.guard()
    const out: ScheduleView[] = []
    for (const w of this.host.workflows()) {
      const v = w.versions[w.versions.length - 1]
      if (!v) continue
      const last = [...this.host.runs().values()].filter((r) => r.view.workflow === w.name).sort((a, b) => b.createdAt - a.createdAt)[0]
      for (const t of (v.doc.triggers ?? []) as { kind?: string; cron?: string; source?: string }[]) {
        out.push({
          workflow: w.name,
          version: v.version,
          trigger_kind: String(t.kind ?? 'unknown'),
          ...(t.cron ? { cron: t.cron } : {}),
          ...(t.source ? { source: t.source } : {}),
          enabled: w.enabled && this.schedulerEnabled,
          runnable: v.runnable,
          ...(last ? { last_run_at: last.view.created_at, last_run_state: last.view.state as RunState } : {}),
        })
      }
    }
    return out
  }

  setWorkflowEnabled(name: string, enabled: boolean): void {
    this.host.guard()
    const w = this.host.workflows().find((x) => x.name === name)
    if (!w) throw err('workflow.not_found', 'not_found', `no workflow named ${name}`)
    w.enabled = enabled
    this.host.notify({ type: 'schedule', workflow: name, enabled })
  }

  addSchedule(workflow: string, cron: string, key: string): { workflow: string; version: number; approvals: number } {
    this.host.guard()
    if (!parseCron(cron)) throw err('api.invalid_params', 'invalid_argument', 'that is not a five-field cron expression (minute hour day-of-month month day-of-week)')
    const w = this.host.workflows().find((x) => x.name === workflow)
    const v = w?.versions[w.versions.length - 1]
    if (!w || !v) throw err('workflow.not_found', 'not_found', `no workflow named ${workflow}`)
    const doc = clone(v.doc) as WorkflowDoc
    const triggers = (doc.triggers ?? []) as { kind?: string; cron?: string }[]
    if (triggers.some((t) => t.kind === 'cron' && t.cron === cron)) throw err('schedule.exists', 'failed_precondition', `${workflow} already runs on ${cron}`)
    doc.triggers = [...triggers, { kind: 'cron', cron }]
    doc.version = v.version + 1
    const r = this.host.put(doc, key)
    return { workflow, version: r.version, approvals: r.approvals.length }
  }

  // ---------------------------------------------------------------- run defaults

  runtimeGet(workflow?: string): RuntimeSettingsView {
    this.host.guard()
    if (!workflow) return { values: { ...this.defaults }, scope_workflow: null, overridden: [] }
    const o = this.overridesOf(workflow)
    return { values: { ...this.defaults, ...o }, scope_workflow: workflow, overridden: Object.keys(o) as (keyof RuntimeSettings)[] }
  }

  private overridesOf(workflow: string): Partial<RuntimeSettings> {
    let o = this.overrides.get(workflow)
    if (!o) {
      o = {}
      // A workflow's own budgets are overrides of the defaults.
      const w = this.host.workflows().find((x) => x.name === workflow)
      const budgets = (w?.versions[w.versions.length - 1]?.doc.budgets ?? {}) as { wall_ms?: number; bytes?: number }
      if (typeof budgets.wall_ms === 'number') o.wall_minutes = Math.round(budgets.wall_ms / 60_000)
      if (typeof budgets.bytes === 'number') o.bytes_gib = Math.round((budgets.bytes / GIB) * 10) / 10
      if (workflow === 'ridership-forecast') o.retries = 5
      if (workflow === 'district-report') o.approval_expiry_hours = 720
      this.overrides.set(workflow, o)
    }
    return o
  }

  runtimeSet(workflow: string | null, patch: Partial<RuntimeSettings>): RuntimeSettingsView {
    this.host.guard()
    if (workflow) Object.assign(this.overridesOf(workflow), patch)
    else Object.assign(this.defaults, patch)
    this.host.notify({ type: 'settings.runtime', workflow: workflow ?? '*', fields: Object.keys(patch) })
    return this.runtimeGet(workflow ?? undefined)
  }

  runtimeReset(workflow: string, field: keyof RuntimeSettings): RuntimeSettingsView {
    this.host.guard()
    delete this.overridesOf(workflow)[field]
    return this.runtimeGet(workflow)
  }

  // ---------------------------------------------------------------- environments and bridges

  environments(edges: { edge_id: string; name: string; last_sync_at?: string; enrolled_at: string }[], version: string, profile: string): EnvironmentsResult {
    this.host.guard()
    const now = Date.now()
    return {
      scheduler_enabled: this.schedulerEnabled,
      environments: [
        { id: 'local', name: 'This daemon', kind: 'local', connected: true, version, endpoint: 'unix:///run/user/1000/ogun/default/ogund.sock', detail: `profile ${profile}` },
        { id: 'hub-regional', name: 'Regional hub (read-only)', kind: 'hub', connected: true, version: '0.1.0-dev', endpoint: 'https://hub.example.org', detail: 'API token, acts as an agent', last_seen_at: iso(now - 40 * 1000) },
        ...edges.map((e, i) => ({
          id: e.edge_id,
          name: e.name,
          kind: 'edge' as const,
          connected: i === 0,
          detail: `enrolled ${iso(new Date(e.enrolled_at).getTime()).slice(0, 10)}`,
          ...(e.last_sync_at ? { last_seen_at: e.last_sync_at } : {}),
          ...(i === 0 ? {} : { error: 'no sync for 62 minutes; the gateway may be asleep or off Wi-Fi' }),
        })),
      ],
      bridges: [
        { id: 'ingest', name: 'Stream ingest (ogun-ingest)', state: 'running', detail: 'MQTT mqtts://gateway.local:8883 · 6 stations · 0 invalid records' },
        { id: 'sync', name: 'Edge-to-hub sync', state: 'running', detail: `accepting signed offers from ${edges.length} enrolled edges` },
        { id: 'server', name: 'Server (ogun-server)', state: 'running', detail: 'behind a reverse proxy at https://hub.example.org · OIDC sign-in · 3 users' },
        { id: 'zeus', name: 'Zeus session bridge', state: 'not_configured', detail: 'Browser-session sources go through zeusd as Zeus jobs. Not linked on this machine.' },
        { id: 'remote_approvals', name: 'Remote approvals (WebAuthn)', state: 'locked', detail: 'Off. ADR 0010 requires an external security review before this can be enabled.' },
      ],
    }
  }

  // ---------------------------------------------------------------- sources

  sourcesGet(): SourcesResult {
    this.host.guard()
    return clone(this.sources)
  }

  sourcesSetRepo(id: string, patch: { watch?: boolean; import_mode?: 'store' | 'store_validate' }): SourcesResult {
    const r = this.sources.repos.find((x) => x.id === id)
    if (!r) throw err('source.not_found', 'not_found', `no source ${id}`)
    Object.assign(r, patch)
    return this.sourcesGet()
  }

  sourcesSync(id: string): SourcesResult {
    const r = this.sources.repos.find((x) => x.id === id)
    if (!r) throw err('source.not_found', 'not_found', `no source ${id}`)
    r.last_sync_at = iso(Date.now())
    r.last_result = `${r.workflows} workflows up to date`
    return this.sourcesGet()
  }

  sourcesSetProvider(id: string, enabled: boolean): SourcesResult {
    const p = this.sources.providers.find((x) => x.id === id)
    if (!p) throw err('source.not_found', 'not_found', `no provider ${id}`)
    if (p.state !== 'available') throw err('source.provider_unavailable', 'failed_precondition', p.detail)
    p.enabled = enabled
    return this.sourcesGet()
  }

  sourcesAutoPull(on: boolean): SourcesResult {
    this.sources.auto_pull = on
    return this.sourcesGet()
  }

  // ---------------------------------------------------------------- evidence

  evidenceGet(): EvidenceSettings {
    return { ...this.evidence }
  }

  evidenceSet(patch: Partial<EvidenceSettings>): EvidenceSettings {
    Object.assign(this.evidence, patch)
    return this.evidenceGet()
  }

  // ---------------------------------------------------------------- storage

  storageInfo(): StorageInfo {
    this.host.guard()
    return {
      root: this.rules.store_location,
      artifacts: { count: Math.round(this.store.artifacts), bytes: Math.round(this.store.bytes) },
      runs: { count: this.store.runs },
      caches: { bytes: Math.round(this.store.caches) },
      limit_bytes: this.rules.limit_gib * GIB,
      retention_days: this.rules.retention_days,
      by_workflow: this.store.byWorkflow.map((w) => ({ workflow: w.workflow, artifacts: Math.round(w.artifacts), bytes: Math.round(w.bytes) })),
    }
  }

  storageRules(): StorageRules {
    return { ...this.rules }
  }

  storageSetRules(patch: Partial<StorageRules>): StorageRules {
    if (patch.keep_lineage_referenced === false) throw err('storage.locked', 'failed_precondition', 'An artifact that later lineage reads is never removed; this rule cannot be turned off.')
    Object.assign(this.rules, patch)
    return this.storageRules()
  }

  storageClearCaches(): StorageInfo {
    this.store.caches = 0
    return this.storageInfo()
  }

  storageCleanupPlan(): CleanupPlan {
    const lines: CleanupPlan['lines'] = []
    const scale = (days: number, unit: number) => (days > 0 ? Math.round(unit * (60 / Math.max(days, 1))) : 0)
    const add = (rule: string, days: number, unit: number, gib: number) => {
      if (days <= 0) return
      const artifacts = Math.min(scale(days, unit), Math.round(this.store.artifacts * 0.2))
      lines.push({ rule, artifacts, bytes: Math.round(artifacts * (gib / unit) * GIB) })
    }
    add(`Failed runs older than ${this.rules.delete_failed_after_days} days`, this.rules.delete_failed_after_days, 40, 0.5)
    add(`Cancelled runs older than ${this.rules.delete_cancelled_after_days} days`, this.rules.delete_cancelled_after_days, 12, 0.2)
    add(`Partial transfers older than ${this.rules.delete_partials_after_days} days`, this.rules.delete_partials_after_days, 6, 0.4)
    add(`Finished runs older than ${this.rules.retention_days} days, not read by later lineage`, this.rules.retention_days, 90, 1.6)
    const logs = this.rules.delete_logs_after_days > 0 ? Math.round(300 * (14 / this.rules.delete_logs_after_days)) : 0
    const cachesBytes = this.rules.clear_caches_after_days > 0 ? Math.min(this.store.caches, 2.4 * GIB * (30 / this.rules.clear_caches_after_days)) : 0
    return { artifacts: lines.reduce((n, l) => n + l.artifacts, 0), bytes: lines.reduce((n, l) => n + l.bytes, 0), caches_bytes: Math.round(cachesBytes), logs, lines }
  }

  storageCleanup(): StorageInfo {
    const plan = this.storageCleanupPlan()
    this.store.artifacts = Math.max(0, this.store.artifacts - plan.artifacts)
    this.store.bytes = Math.max(0, this.store.bytes - plan.bytes)
    this.store.caches = Math.max(0, this.store.caches - plan.caches_bytes)
    const share = plan.bytes / Math.max(1, this.store.byWorkflow.reduce((n, w) => n + w.bytes, 0))
    for (const w of this.store.byWorkflow) {
      w.bytes = Math.max(0, w.bytes * (1 - share))
      w.artifacts = Math.max(0, w.artifacts * (1 - share))
    }
    this.host.notify({ type: 'storage.cleanup', artifacts: plan.artifacts })
    return this.storageInfo()
  }

  // ---------------------------------------------------------------- archive

  archiveList(): ArchiveItem[] {
    return clone(this.archived)
  }

  archiveRestore(id: string): void {
    const i = this.archived.findIndex((x) => x.id === id)
    if (i < 0) throw err('archive.not_found', 'not_found', 'that item is no longer archived')
    this.archived.splice(i, 1)
  }

  // ---------------------------------------------------------------- the provider ladder

  ladderGet(): LadderProvider[] {
    return clone(this.ladder)
  }

  ladderPut(providers: LadderProvider[]): LadderProvider[] {
    this.ladder = clone(providers)
    return this.ladderGet()
  }

  ladderSpend() {
    return { month_usd_micro: 1_384_200, cap_usd_micro: 25_000_000, entries: 212 }
  }
}

export type { ConnectionValue }
