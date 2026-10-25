// Helpers shared by the scripted demo: deterministic pseudo-randomness, ids, time, error envelopes and partition keys.

import type { NodeState, OgunError, PartitionRange, PartitionState } from '../api/types'

export const SEC = 1000
export const MIN = 60 * SEC
export const HOUR = 60 * MIN
export const DAY = 24 * HOUR
export const TICK_MS = 3000
export const WINDOW_EVERY_MS = 30_000
export const RESTART_MS = 1800

// ---------------------------------------------------------------- deterministic bits

export function fnv(s: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

export function rng(seed: string): () => number {
  let x = fnv(seed) || 0x9e3779b9
  return () => {
    x ^= x << 13
    x >>>= 0
    x ^= x >>> 17
    x ^= x << 5
    x >>>= 0
    return x
  }
}

/** A 64-hex-digit string that looks like a BLAKE3 digest; stable for one seed. Demo data, not a real hash. */
export function hex(seed: string, len = 64): string {
  const next = rng(seed)
  let out = ''
  while (out.length < len) out += next().toString(16).padStart(8, '0')
  return out.slice(0, len)
}

export const B32 = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

export function ulid(ts: number, seed: string): string {
  let t = Math.floor(ts)
  let time = ''
  for (let i = 0; i < 10; i++) {
    time = B32.charAt(t % 32) + time
    t = Math.floor(t / 32)
  }
  const next = rng(seed)
  let rand = ''
  for (let i = 0; i < 16; i++) rand += B32.charAt(next() % 32)
  return time + rand
}

export const iso = (ts: number) => new Date(ts).toISOString()
export const pad2 = (n: number) => String(n).padStart(2, '0')

export function monthKeys(from: string, to: string): string[] {
  const [fy, fm] = from.split('-').map(Number)
  const [ty, tm] = to.split('-').map(Number)
  if (!fy || !fm || !ty || !tm) return []
  const out: string[] = []
  let y = fy
  let m = fm
  while (y < ty || (y === ty && m <= tm)) {
    out.push(`${y}-${pad2(m)}`)
    m++
    if (m > 12) {
      m = 1
      y++
    }
  }
  return out
}

export function dayKeys(from: string, to: string): string[] {
  const out: string[] = []
  const a = new Date(from + 'T00:00:00Z').getTime()
  const b = new Date(to + 'T00:00:00Z').getTime()
  if (Number.isNaN(a) || Number.isNaN(b)) return []
  for (let t = a; t <= b && out.length < 5000; t += DAY) out.push(iso(t).slice(0, 10))
  return out
}

export const monthOf = (ts: number) => iso(ts).slice(0, 7)
export const dayOf = (ts: number) => iso(ts).slice(0, 10)
export const windowKey = (ts: number) => 'w' + iso(ts).slice(0, 16)

export function compress(keys: string[], stateOf: (k: string) => PartitionState): PartitionRange[] {
  const out: PartitionRange[] = []
  for (const k of keys) {
    const s = stateOf(k)
    const last = out[out.length - 1]
    if (last && last.state === s) {
      last.to = k
      last.count++
    } else out.push({ from: k, to: k, count: 1, state: s })
  }
  return out
}

export function clone<T>(v: T): T {
  return structuredClone(v)
}

export function err(code: string, cls: string, message: string, retryable = false, details?: Record<string, unknown>): OgunError {
  return { code, class: cls, message, retryable, ...(details ? { details } : {}) }
}

export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms))

export const TERMINAL: ReadonlySet<NodeState> = new Set(['succeeded', 'failed', 'cancelled', 'skipped'])

