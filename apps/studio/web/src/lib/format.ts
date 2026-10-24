// Formatting for people: times, sizes, counts and hashes. Data stays exact; only presentation changes.

export function fmtTime(ts: string | undefined | null): string {
  if (!ts) return '–'
  const d = new Date(ts)
  if (Number.isNaN(d.getTime())) return ts
  return d.toISOString().replace('T', ' ').replace(/\.\d{3}Z$/, 'Z')
}

export function fmtAgo(ts: string | undefined | null, now = Date.now()): string {
  if (!ts) return '–'
  const t = new Date(ts).getTime()
  if (Number.isNaN(t)) return ts
  const s = Math.round((now - t) / 1000)
  const abs = Math.abs(s)
  const unit = abs < 60 ? [abs, 's'] : abs < 3600 ? [Math.floor(abs / 60), 'min'] : abs < 86400 ? [Math.floor(abs / 3600), 'h'] : [Math.floor(abs / 86400), 'd']
  return s >= 0 ? `${unit[0]} ${unit[1]} ago` : `in ${unit[0]} ${unit[1]}`
}

export function fmtMs(ms: number | undefined | null): string {
  if (ms === undefined || ms === null) return '–'
  return fmtTime(new Date(ms).toISOString())
}

export function fmtBytes(n: number | undefined | null): string {
  if (n === undefined || n === null) return '–'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return i === 0 ? `${n} B` : `${v.toFixed(v < 10 ? 2 : 1)} ${units[i]}`
}

export function fmtCount(n: number | undefined | null): string {
  if (n === undefined || n === null) return '–'
  return n.toLocaleString('en-US')
}

/** First 4 and last 4 hex digits, as the CLI prints next to a full hash shown elsewhere. */
export function shortHash(h: string | undefined | null): string {
  if (!h) return '–'
  return h.length > 12 ? `${h.slice(0, 4)}…${h.slice(-4)}` : h
}

/** A fresh idempotency key for one person's action (O-15 section 5). */
export function idempotencyKey(action: string): string {
  const rnd = crypto.getRandomValues(new Uint8Array(12))
  return `studio-${action}-${Array.from(rnd, (b) => b.toString(16).padStart(2, '0')).join('')}`
}

export function decodeBase64(b64: string): Uint8Array {
  const bin = atob(b64)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/** UTF-8 text if the bytes decode cleanly and look like text, else null. */
export function asText(bytes: Uint8Array): string | null {
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes)
    const sample = text.slice(0, 4096)
    let control = 0
    for (let i = 0; i < sample.length; i++) {
      const c = sample.charCodeAt(i)
      if (c < 9 || (c > 13 && c < 32)) control++
    }
    return control > sample.length / 50 ? null : text
  } catch {
    return null
  }
}

/** Plain-language label for a state or cause, keeping the exact word. */
export function words(s: string): string {
  return s.replace(/_/g, ' ')
}
