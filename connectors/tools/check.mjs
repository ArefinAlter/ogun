// Validates the connector catalog against schema/connector.v0.schema.json and the rules the schema cannot say.
//
//   node connectors/tools/check.mjs            # checks connectors/catalog/*.connector.json; exit 1 on any finding
//
// No dependencies. The schema check is a small validator for exactly the keywords the schema uses. The catalog
// rules (O-16 section 6) are what keep a manifest safe to show and to copy from:
//   1. a field named like a secret is declared `secret: true` (O-02 section 10 names the words);
//   2. no manifest holds a value for a secret: fields carry names, never defaults or examples;
//   3. a domain template names only settings or fields the manifest declares;
//   4. operator names are unique across the catalog;
//   5. a `built` or `verified` connector names the code that implements it, and the path exists.

import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
export const ROOT = resolve(here, '..')
const REPO = resolve(ROOT, '..')

/** O-02 section 10: names that mean "this holds a secret". */
export const SECRETISH = /^(access_key|access_token|api[-_]?key|apikey|auth_token|authorization|client_secret|cookie|cookies|password|passwd|private_key|refresh_token|secret|secret_key|session_token|token|x-api-key|x-auth-token|credentials_json|sas_token|connection_string|webhook_url|signing_secret|app_key)$/i
const SECRETISH_PART = /(password|secret|token|api_?key|private_key|credentials?)/i

// ---------------------------------------------------------------- a small JSON Schema validator

function typeOf(v) {
  if (v === null) return 'null'
  if (Array.isArray(v)) return 'array'
  if (Number.isInteger(v)) return 'integer'
  return typeof v
}

function typeMatches(want, v) {
  const t = typeOf(v)
  return want === t || (want === 'number' && t === 'integer')
}

function resolveRef(schema, ref) {
  if (!ref.startsWith('#/')) throw new Error(`unsupported $ref ${ref}`)
  return ref
    .slice(2)
    .split('/')
    .reduce((node, key) => node[key], schema)
}

export function validate(schema, value, node = schema, path = '', out = []) {
  if (node.$ref) return validate(schema, value, resolveRef(schema, node.$ref), path, out)
  const fail = (message) => out.push({ path: path || '/', message })
  if ('const' in node && value !== node.const) return fail(`must be ${JSON.stringify(node.const)}`), out
  if (node.enum && !node.enum.includes(value)) return fail(`must be one of ${node.enum.join(', ')}`), out
  if (node.type) {
    const types = Array.isArray(node.type) ? node.type : [node.type]
    if (!types.some((t) => typeMatches(t, value))) return fail(`must be ${types.join(' or ')}, not ${typeOf(value)}`), out
  }
  if (typeof value === 'string') {
    if (node.minLength !== undefined && value.length < node.minLength) fail(`shorter than ${node.minLength}`)
    if (node.maxLength !== undefined && value.length > node.maxLength) fail(`longer than ${node.maxLength}`)
    if (node.pattern && !new RegExp(node.pattern, 'u').test(value)) fail(`does not match ${node.pattern}`)
  }
  if (typeof value === 'number') {
    if (node.minimum !== undefined && value < node.minimum) fail(`below ${node.minimum}`)
    if (node.maximum !== undefined && value > node.maximum) fail(`above ${node.maximum}`)
  }
  if (Array.isArray(value)) {
    if (node.minItems !== undefined && value.length < node.minItems) fail(`fewer than ${node.minItems} items`)
    if (node.uniqueItems && new Set(value.map((v) => JSON.stringify(v))).size !== value.length) fail('items must be unique')
    if (node.items) value.forEach((v, i) => validate(schema, v, node.items, `${path}/${i}`, out))
  }
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    for (const key of node.required ?? []) if (!(key in value)) out.push({ path: `${path}/${key}`, message: 'is required' })
    const props = node.properties ?? {}
    for (const [key, v] of Object.entries(value)) {
      if (key in props) validate(schema, v, props[key], `${path}/${key}`, out)
      else if (node.additionalProperties === false) out.push({ path: `${path}/${key}`, message: 'is not allowed' })
    }
  }
  return out
}

// ---------------------------------------------------------------- the catalog rules

function placeholders(domain) {
  return [...domain.matchAll(/\{([a-z_][a-z0-9_]*)\}/g)].map((m) => m[1])
}

export function rules(manifest) {
  const out = []
  const add = (path, message) => out.push({ path, message })
  const names = new Set()
  manifest.settings?.forEach((s, i) => {
    names.add(s.name)
    if (SECRETISH.test(s.name) || SECRETISH_PART.test(s.name)) add(`/settings/${i}/name`, `${s.name} looks like a secret; settings are plain values, so make it an auth field with secret: true`)
    if (s.type === 'enum' && !s.options) add(`/settings/${i}`, 'an enum setting lists its options')
    if (s.type !== 'enum' && s.options) add(`/settings/${i}/options`, 'only an enum setting has options')
    if (s.type === 'enum' && s.default !== undefined && !s.options?.includes(s.default)) add(`/settings/${i}/default`, 'the default is not one of the options')
  })
  const authIds = new Set()
  manifest.auth?.forEach((m, i) => {
    if (authIds.has(m.id)) add(`/auth/${i}/id`, `duplicate auth method ${m.id}`)
    authIds.add(m.id)
    m.fields.forEach((f, j) => {
      names.add(f.name)
      const looksSecret = SECRETISH.test(f.name) || SECRETISH_PART.test(f.name)
      if (looksSecret && !f.secret) add(`/auth/${i}/fields/${j}/secret`, `${f.name} looks like a secret and must be declared secret: true`)
      if (f.secret && 'default' in f) add(`/auth/${i}/fields/${j}`, 'a secret field has no default')
    })
  })
  for (const [i, d] of (manifest.egress?.domains ?? []).entries()) {
    for (const p of placeholders(d)) if (!names.has(p)) add(`/egress/domains/${i}`, `{${p}} is not a setting or field of this connector`)
  }
  if (manifest.maturity !== 'contract') {
    const path = manifest.implementation?.path
    if (!path) add('/implementation', `a ${manifest.maturity} connector names the code that implements it`)
    else if (!existsSync(join(REPO, path))) add('/implementation/path', `${path} does not exist`)
  } else if (manifest.implementation) add('/implementation', 'a contract-only connector has no implementation')
  if (manifest.egress && manifest.egress.domains.length === 0 && manifest.egress.ports.length > 0 && !manifest.directions?.includes('trigger') && !manifest.directions?.includes('edge'))
    add('/egress/domains', 'a connector that reaches an outside system declares where')
  const ops = new Set()
  manifest.operators?.forEach((o, i) => {
    if (ops.has(o.op)) add(`/operators/${i}/op`, `duplicate operator ${o.op}`)
    ops.add(o.op)
    if (o.kind === 'adapter' && !o.via) add(`/operators/${i}/via`, 'an adapter says how it reaches the system (rest, cli, sdk or driver)')
    if (o.via === 'cli' && !o.tool) add(`/operators/${i}/tool`, 'a cli adapter names the tool')
  })
  return out
}

export function loadSchema() {
  return JSON.parse(readFileSync(join(ROOT, 'schema', 'connector.v0.schema.json'), 'utf8'))
}

export function loadCatalog() {
  const dir = join(ROOT, 'catalog')
  return readdirSync(dir)
    .filter((f) => f.endsWith('.connector.json'))
    .sort()
    .map((f) => ({ file: f, manifest: JSON.parse(readFileSync(join(dir, f), 'utf8')) }))
}

/** Every finding for the whole catalog, as `file: path: message`. */
export function checkAll(entries = loadCatalog(), schema = loadSchema()) {
  const findings = []
  const ids = new Map()
  const ops = new Map()
  for (const { file, manifest } of entries) {
    const tag = (f) => `${file}: ${f.path}: ${f.message}`
    for (const f of validate(schema, manifest)) findings.push(tag(f))
    if (typeof manifest.id === 'string') {
      if (file !== `${manifest.id}.connector.json`) findings.push(`${file}: /id: the file is named after the id (${manifest.id}.connector.json)`)
      if (ids.has(manifest.id)) findings.push(`${file}: /id: also used by ${ids.get(manifest.id)}`)
      ids.set(manifest.id, file)
    }
    if (Array.isArray(manifest.operators) && Array.isArray(manifest.auth) && Array.isArray(manifest.settings) && manifest.egress)
      for (const f of rules(manifest)) findings.push(tag(f))
    for (const o of manifest.operators ?? []) {
      if (typeof o.op !== 'string') continue
      if (ops.has(o.op)) findings.push(`${file}: /operators: ${o.op} is also defined by ${ops.get(o.op)}`)
      ops.set(o.op, file)
    }
  }
  return findings
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const entries = loadCatalog()
  const findings = checkAll(entries)
  if (findings.length) {
    for (const f of findings) console.error(f)
    console.error(`\n${findings.length} finding(s) in ${entries.length} manifest(s)`)
    process.exit(1)
  }
  const byMaturity = entries.reduce((m, e) => ((m[e.manifest.maturity] = (m[e.manifest.maturity] ?? 0) + 1), m), {})
  const ops = entries.reduce((n, e) => n + e.manifest.operators.length, 0)
  console.log(`${entries.length} connectors, ${ops} operators, all valid (${Object.entries(byMaturity).map(([k, v]) => `${v} ${k}`).join(', ')})`)
}
