// The provider ladder file `<state_dir>/llm.json` (O-03 section 8.1): its entries, the rules the daemon's parser
// applies, and the document Studio writes for a person to place there. The rules mirror `ogun-llm`'s
// `LadderConfig` validation; the daemon remains the authority and refuses a document that breaks them.

import type { LadderProvider } from '../api/types'

export const KINDS: LadderProvider['kind'][] = ['anthropic', 'openai', 'openai_compatible', 'ollama', 'google']

export const DEFAULT_BASE: Partial<Record<LadderProvider['kind'], string>> = {
  anthropic: 'https://api.anthropic.com',
  openai: 'https://api.openai.com/v1',
  ollama: 'http://127.0.0.1:11434',
  google: 'https://generativelanguage.googleapis.com/v1beta',
}

export const NEEDS_KEY: Record<LadderProvider['kind'], boolean> = { anthropic: true, openai: true, google: true, openai_compatible: false, ollama: false }

export function defaultCapabilities(kind: LadderProvider['kind']): LadderProvider['capabilities'] {
  if (kind === 'anthropic') return ['chat', 'json_schema']
  if (kind === 'openai_compatible') return ['chat']
  return ['chat', 'json_schema', 'embeddings']
}

export function isLoopback(host: string): boolean {
  return host === 'localhost' || host === '127.0.0.1' || host === '[::1]' || host === '::1'
}

export interface Issue {
  index: number
  field: string
  message: string
}

const NAME = /^[a-z][a-z0-9_-]{0,63}$/
const MODEL = /^[A-Za-z0-9._:/@+-]{1,200}$/
const SECRET = /^[A-Za-z0-9._-]{1,128}$/

export function validateLadder(list: LadderProvider[]): Issue[] {
  const out: Issue[] = []
  if (list.length > 32) out.push({ index: -1, field: 'providers', message: 'A ladder has at most 32 entries.' })
  const seen = new Set<string>()
  list.forEach((p, i) => {
    const add = (field: string, message: string) => out.push({ index: i, field, message })
    if (!NAME.test(p.name)) add('name', 'A name is lower case letters, digits, hyphens and underscores, starting with a letter (up to 64).')
    else if (seen.has(p.name)) add('name', `Another entry is already called ${p.name}.`)
    seen.add(p.name)
    if (p.enabled || p.model) {
      if (!MODEL.test(p.model)) add('model', p.model ? 'A model id uses letters, digits and . _ : / @ + - (up to 200).' : 'An enabled entry names its model.')
    }
    let url: URL | null = null
    try {
      url = new URL(p.base_url)
    } catch {
      add('base_url', 'Not a URL.')
    }
    if (url) {
      if (url.username || url.password || url.search || url.hash) add('base_url', 'No user name, password, query or fragment in a base URL.')
      if (url.protocol === 'http:' && !isLoopback(url.hostname)) {
        if (!p.allow_insecure_http) add('base_url', 'Plain http is accepted only for a loopback host, unless allow_insecure_http is set.')
        else if (p.api_key_secret) add('base_url', 'Plain http to another host cannot carry an API key.')
      } else if (url.protocol !== 'http:' && url.protocol !== 'https:') add('base_url', 'Use https, or http to a loopback host.')
    }
    if (NEEDS_KEY[p.kind] && p.enabled && !p.api_key_secret) add('api_key_secret', `${p.kind} needs the name of the secret that holds its key.`)
    if (p.api_key_secret && !SECRET.test(p.api_key_secret)) add('api_key_secret', 'A secret name has letters, digits, dots, hyphens and underscores.')
    for (const f of ['input_per_mtok_usd_micro', 'output_per_mtok_usd_micro'] as const) if (!Number.isInteger(p[f]) || p[f] < 0) add(f, 'A price is a whole number of micro-dollars per million tokens.')
    if (!Number.isInteger(p.max_output_tokens) || p.max_output_tokens < 1 || p.max_output_tokens > 1_000_000) add('max_output_tokens', '1 to 1 000 000.')
    if (!Number.isInteger(p.timeout_ms) || p.timeout_ms < 1000 || p.timeout_ms > 600_000) add('timeout_ms', '1 000 to 600 000 milliseconds.')
    if (p.capabilities.length === 0) add('capabilities', 'At least one capability.')
    if (p.capabilities.includes('json_schema') && !p.capabilities.includes('chat')) add('capabilities', 'json_schema needs chat.')
    if (p.kind === 'anthropic' && p.capabilities.includes('embeddings')) add('capabilities', 'Anthropic has no embeddings.')
  })
  return out
}

/** The document for `llm.json`: members in the order the spec lists them, optional ones only when set. */
export function ladderDocument(list: LadderProvider[]): string {
  const providers = list.map((p) => {
    const o: Record<string, unknown> = { name: p.name, kind: p.kind }
    if (!p.enabled) o.enabled = false
    o.base_url = p.base_url
    o.model = p.model
    if (p.api_key_secret) o.api_key_secret = p.api_key_secret
    o.input_per_mtok_usd_micro = p.input_per_mtok_usd_micro
    o.output_per_mtok_usd_micro = p.output_per_mtok_usd_micro
    o.max_output_tokens = p.max_output_tokens
    if (p.timeout_ms !== 120_000) o.timeout_ms = p.timeout_ms
    const def = defaultCapabilities(p.kind)
    if (p.capabilities.join() !== def.join()) o.capabilities = p.capabilities
    if (p.allow_insecure_http) o.allow_insecure_http = true
    return o
  })
  return JSON.stringify({ version: 1, providers }, null, 2)
}

/** Reads what a daemon reports as its ladder, tolerating missing members. */
export function readLadder(ladder: unknown): LadderProvider[] | null {
  if (!ladder || typeof ladder !== 'object') return null
  const providers = (ladder as { providers?: unknown }).providers
  if (!Array.isArray(providers)) return null
  const out: LadderProvider[] = []
  for (const raw of providers) {
    if (!raw || typeof raw !== 'object') continue
    const p = raw as Record<string, unknown>
    const kind = KINDS.includes(p.kind as LadderProvider['kind']) ? (p.kind as LadderProvider['kind']) : 'openai_compatible'
    out.push({
      name: String(p.name ?? ''),
      kind,
      enabled: p.enabled !== false,
      base_url: typeof p.base_url === 'string' ? p.base_url : (DEFAULT_BASE[kind] ?? ''),
      model: typeof p.model === 'string' ? p.model : '',
      ...(typeof p.api_key_secret === 'string' ? { api_key_secret: p.api_key_secret } : {}),
      input_per_mtok_usd_micro: Number(p.input_per_mtok_usd_micro ?? 0),
      output_per_mtok_usd_micro: Number(p.output_per_mtok_usd_micro ?? 0),
      max_output_tokens: Number(p.max_output_tokens ?? 8192),
      timeout_ms: Number(p.timeout_ms ?? 120_000),
      capabilities: Array.isArray(p.capabilities) ? (p.capabilities.filter((c) => c === 'chat' || c === 'json_schema' || c === 'embeddings') as LadderProvider['capabilities']) : defaultCapabilities(kind),
      allow_insecure_http: p.allow_insecure_http === true,
    })
  }
  return out
}

export function usd(micro: number): string {
  return (micro / 1_000_000).toFixed(micro % 10_000 === 0 ? 2 : 4)
}

export function toMicro(dollars: string): number {
  const n = Number(dollars)
  return Number.isFinite(n) && n >= 0 ? Math.round(n * 1_000_000) : 0
}
