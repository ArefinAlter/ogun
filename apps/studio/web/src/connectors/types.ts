// The connector manifest as Studio reads it: `connectors/schema/connector.v0.schema.json` (O-16) in TypeScript.
// A manifest holds names and plain settings, never a secret.

export type Category =
  | 'warehouse'
  | 'database'
  | 'object_storage'
  | 'streaming'
  | 'orchestration'
  | 'observability'
  | 'catalog'
  | 'quality'
  | 'notification'
  | 'source'
  | 'edge'
  | 'agent'

export type Maturity = 'contract' | 'built' | 'verified'
export type Direction = 'source' | 'sink' | 'emit' | 'trigger' | 'edge'
export type Region = 'global' | 'cn'
export type Protocol = 'rest' | 's3_api' | 'sql_wire' | 'kafka_api' | 'webhook' | 'otlp' | 'mqtt' | 'cli' | 'sdk' | 'file' | 'device'

export interface AuthField {
  name: string
  label: string
  secret: boolean
  required?: boolean
  help?: string
}

export interface AuthMethod {
  id: string
  label: string
  fields: AuthField[]
}

export interface Setting {
  name: string
  label: string
  type: 'string' | 'integer' | 'boolean' | 'enum'
  required?: boolean
  default?: string | number | boolean
  options?: string[]
  placeholder?: string
  help?: string
}

export interface ConnectorOperator {
  op: string
  kind: 'adapter' | 'builtin'
  summary: string
  inputs?: string[]
  outputs: { name: string; media_type?: string }[]
  determinism: 'pure' | 'seeded' | 'nondeterministic'
  via?: 'rest' | 'cli' | 'sdk' | 'driver' | 'builtin'
  tool?: string
}

export interface ConnectorManifest {
  connector_version: 0
  id: string
  name: string
  vendor: string
  category: Category
  regions: Region[]
  summary: string
  docs_url: string
  maturity: Maturity
  directions: Direction[]
  protocol: Protocol
  auth: AuthMethod[]
  settings: Setting[]
  egress: { domains: string[]; ports: number[]; note?: string }
  operators: ConnectorOperator[]
  approvals?: ('egress' | 'code_change' | 'budget' | 'publish')[]
  implementation?: { path: string; evidence?: string }
  notes?: string
}
