// What Studio does with the connector catalog: labels, search, the domains a connection needs, the workflow node
// snippet, and the lazy loader (the catalog is a separate chunk, loaded when a page needs it).

import { useEffect, useState } from 'preact/hooks'

import type { Category, ConnectorManifest, ConnectorOperator, Direction, Maturity, Region } from './types'

export type { ConnectorManifest, Category, Maturity, Direction, Region } from './types'

export const CATEGORIES: { id: Category; label: string; blurb: string }[] = [
  { id: 'warehouse', label: 'Warehouses and lakehouses', blurb: 'Query results in, Parquet partitions out.' },
  { id: 'database', label: 'Databases', blurb: 'Relational, document and embedded stores.' },
  { id: 'object_storage', label: 'Object storage', blurb: 'Buckets, S3-compatible stores and file transfer.' },
  { id: 'streaming', label: 'Streaming', blurb: 'Event logs and message buses.' },
  { id: 'orchestration', label: 'Orchestrators', blurb: 'Systems that call Ogun and can never approve.' },
  { id: 'observability', label: 'Observability', blurb: 'Metrics, events and logs out of Ogun.' },
  { id: 'catalog', label: 'Lineage and catalogs', blurb: 'OpenLineage events and table registration.' },
  { id: 'quality', label: 'Data quality', blurb: 'Checks that run as sandboxed operators.' },
  { id: 'notification', label: 'Notifications', blurb: 'Chat, paging and mail, including DingTalk, Feishu and WeCom.' },
  { id: 'source', label: 'Public data', blurb: 'Open datasets and developer platforms.' },
  { id: 'edge', label: 'Edge and devices', blurb: 'Sensors and gateways that push into Ogun.' },
  { id: 'agent', label: 'Editors and agents', blurb: 'MCP, VS Code and the language server.' },
]

export const MATURITY: Record<Maturity, { label: string; long: string }> = {
  contract: {
    label: 'Manifest only',
    long: 'The manifest says what Ogun would connect to and what it would contribute. No operator for this system exists yet, and nothing has been run against it.',
  },
  built: {
    label: 'Built',
    long: 'The code exists and passes its own tests. It has not been run against the real service or device.',
  },
  verified: {
    label: 'Verified',
    long: 'Run end to end against the real service, with the evidence cited.',
  },
}

export const DIRECTIONS: Record<Direction, string> = {
  source: 'Reads from it',
  sink: 'Writes to it',
  emit: 'Sends telemetry to it',
  trigger: 'Calls Ogun',
  edge: 'Devices push in',
}

export const REGION_LABEL: Record<Region, string> = { global: 'Global', cn: 'China' }

export function categoryLabel(c: Category): string {
  return CATEGORIES.find((x) => x.id === c)?.label ?? c
}

// ---------------------------------------------------------------- loading

let loaded: Promise<ConnectorManifest[]> | null = null

/** The catalog, as its own chunk. */
export function loadCatalog(): Promise<ConnectorManifest[]> {
  loaded ??= import('./catalog.generated').then((m) => m.CATALOG)
  return loaded
}

/** The catalog, or undefined while it loads. */
export function useCatalog(): ConnectorManifest[] | undefined {
  const [catalog, setCatalog] = useState<ConnectorManifest[] | undefined>(undefined)
  useEffect(() => {
    let live = true
    void loadCatalog().then((c) => {
      if (live) setCatalog(c)
    })
    return () => {
      live = false
    }
  }, [])
  return catalog
}

// ---------------------------------------------------------------- search and filter

export interface CatalogFilter {
  query: string
  category: Category | ''
  region: Region | ''
  maturity: Maturity | ''
}

export function matchesFilter(m: ConnectorManifest, f: CatalogFilter): boolean {
  if (f.category && m.category !== f.category) return false
  if (f.region && !m.regions.includes(f.region)) return false
  if (f.maturity && m.maturity !== f.maturity) return false
  const q = f.query.trim().toLowerCase()
  if (!q) return true
  const haystack = [m.id, m.name, m.vendor, m.summary, categoryLabel(m.category), ...m.operators.map((o) => o.op), ...m.egress.domains].join(' ').toLowerCase()
  return q.split(/\s+/).every((word) => haystack.includes(word))
}

// ---------------------------------------------------------------- what a connection needs

type Values = Record<string, string | number | boolean>

/** The hosts a connection will contact: the manifest's domains with the person's settings filled in. */
export function expandDomains(m: ConnectorManifest, values: Values): { host: string; complete: boolean }[] {
  return m.egress.domains.map((d) => {
    let complete = true
    const host = d.replace(/\{([a-z_][a-z0-9_]*)\}/g, (whole, name: string) => {
      const v = values[name] ?? m.settings.find((s) => s.name === name)?.default
      if (v === undefined || v === '') {
        complete = false
        return whole
      }
      return String(v)
    })
    return { host, complete }
  })
}

/** The secret name proposed for an auth field of a connection. */
export function defaultSecretName(connection: string, field: string): string {
  const base = `${connection || 'connection'}_${field}`.toLowerCase().replace(/[^a-z0-9._-]+/g, '_')
  return base.slice(0, 128)
}

export const CONNECTION_NAME = /^[a-z][a-z0-9_-]{0,63}$/
export const SECRET_NAME = /^[A-Za-z0-9._-]{1,128}$/

function placeholderValue(m: ConnectorManifest, name: string): string | number | boolean {
  const s = m.settings.find((x) => x.name === name)
  if (!s) return `<${name}>`
  if (s.default !== undefined) return s.default
  if (s.placeholder) return s.placeholder
  if (s.type === 'boolean') return false
  if (s.type === 'integer') return 0
  return `<${name}>`
}

/** An illustrative workflow node for one operator of a connector, with secrets as `secret_ref`s. */
export function nodeSnippet(m: ConnectorManifest, op: ConnectorOperator, opts: { authId?: string; values?: Values; secretRefs?: Record<string, string>; connection?: string } = {}): string {
  const auth = m.auth.find((a) => a.id === opts.authId) ?? m.auth[0]
  const config: Record<string, unknown> = {}
  for (const s of m.settings) {
    const v = opts.values?.[s.name]
    if (v !== undefined && v !== '') config[s.name] = v
    else if (s.required || s.default !== undefined) config[s.name] = placeholderValue(m, s.name)
  }
  for (const f of auth?.fields ?? []) {
    if (!f.secret) {
      if (f.required) config[f.name] = opts.values?.[f.name] ?? `<${f.name}>`
      continue
    }
    if (!f.required && !opts.secretRefs?.[f.name]) continue
    config[f.name] = { secret_ref: opts.secretRefs?.[f.name] ?? defaultSecretName(opts.connection ?? m.id.replace(/-/g, '_'), f.name) }
  }
  const node: Record<string, unknown> = { id: op.op.split('@')[0]?.replace(/\./g, '_') ?? 'step', op: op.op }
  if (op.inputs?.length) node.inputs = op.inputs.map((i) => `previous.${i}`)
  node.config = config
  return JSON.stringify(node, null, 2)
}

/** The command that stores one secret value on a terminal. */
export function secretCommand(name: string): string {
  return `ogun secrets set ${name}`
}
