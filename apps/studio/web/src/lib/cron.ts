// Five-field cron in UTC, as the Ogun scheduler reads it (O-04): minute hour day-of-month month day-of-week,
// with `*`, numbers, ranges `a-b`, lists `a,b` and steps `*/n` or `a-b/n`. As in Vixie cron, when both
// day-of-month and day-of-week are restricted a day matches if EITHER does; day-of-week 7 means Sunday.
// This is a presentation helper (the next fire time on the Schedules page); the daemon decides what runs.

interface Field {
  any: boolean
  values: Set<number>
}

const RANGES: [number, number][] = [
  [0, 59],
  [0, 23],
  [1, 31],
  [1, 12],
  [0, 7],
]

function parseField(text: string, [lo, hi]: [number, number]): Field | null {
  const values = new Set<number>()
  let any = false
  for (const part of text.split(',')) {
    const [rangeText, stepText] = part.split('/') as [string, string | undefined]
    const step = stepText === undefined ? 1 : Number(stepText)
    if (!Number.isInteger(step) || step < 1) return null
    let from: number
    let to: number
    if (rangeText === '*') {
      from = lo
      to = hi
      if (stepText === undefined && text === '*') any = true
    } else if (/^\d+-\d+$/.test(rangeText)) {
      const [a, b] = rangeText.split('-').map(Number) as [number, number]
      from = a
      to = b
    } else if (/^\d+$/.test(rangeText)) {
      from = Number(rangeText)
      to = stepText === undefined ? from : hi
    } else return null
    if (from < lo || to > hi || from > to) return null
    for (let v = from; v <= to; v += step) values.add(v)
  }
  return { any, values }
}

export interface Cron {
  minute: Field
  hour: Field
  dom: Field
  month: Field
  dow: Field
}

export function parseCron(expr: string): Cron | null {
  const parts = expr.trim().split(/\s+/)
  if (parts.length !== 5) return null
  const fields = parts.map((p, i) => parseField(p, RANGES[i] as [number, number]))
  if (fields.some((f) => f === null)) return null
  const [minute, hour, dom, month, dow] = fields as [Field, Field, Field, Field, Field]
  if (dow.values.has(7)) dow.values.add(0)
  return { minute, hour, dom, month, dow }
}

function dayMatches(c: Cron, d: Date): boolean {
  const domOk = c.dom.values.has(d.getUTCDate())
  const dowOk = c.dow.values.has(d.getUTCDay())
  if (c.dom.any && c.dow.any) return true
  if (c.dom.any) return dowOk
  if (c.dow.any) return domOk
  return domOk || dowOk
}

/** The first fire time strictly after `from`, or null when the expression is invalid or never fires within 8 years. */
export function nextFire(expr: string, from: Date): Date | null {
  const c = parseCron(expr)
  if (!c) return null
  const t = new Date(Date.UTC(from.getUTCFullYear(), from.getUTCMonth(), from.getUTCDate(), from.getUTCHours(), from.getUTCMinutes() + 1, 0, 0))
  const limit = from.getTime() + 8 * 366 * 86_400_000
  while (t.getTime() <= limit) {
    if (!c.month.values.has(t.getUTCMonth() + 1)) {
      t.setUTCMonth(t.getUTCMonth() + 1, 1)
      t.setUTCHours(0, 0, 0, 0)
      continue
    }
    if (!dayMatches(c, t)) {
      t.setUTCDate(t.getUTCDate() + 1)
      t.setUTCHours(0, 0, 0, 0)
      continue
    }
    if (!c.hour.values.has(t.getUTCHours())) {
      t.setUTCHours(t.getUTCHours() + 1, 0, 0, 0)
      continue
    }
    if (!c.minute.values.has(t.getUTCMinutes())) {
      t.setUTCMinutes(t.getUTCMinutes() + 1, 0, 0)
      continue
    }
    return t
  }
  return null
}

const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday']

function hhmm(c: Cron): string | null {
  if (c.minute.values.size !== 1 || c.hour.values.size !== 1) return null
  const [m] = [...c.minute.values] as [number]
  const [h] = [...c.hour.values] as [number]
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`
}

/** A plain-words reading of the common shapes; anything else is returned as written. */
export function describeCron(expr: string): string {
  const c = parseCron(expr)
  if (!c) return expr
  const at = hhmm(c)
  if (at && c.month.any && c.dom.any && c.dow.any) return `Every day at ${at} UTC`
  if (at && c.month.any && c.dom.values.size === 1 && c.dow.any) {
    const [d] = [...c.dom.values] as [number]
    return `On day ${d} of every month at ${at} UTC`
  }
  if (at && c.month.any && c.dom.any && c.dow.values.size === 1) {
    const [w] = [...c.dow.values] as [number]
    return `Every ${DAYS[w % 7]} at ${at} UTC`
  }
  if (c.hour.any && c.dom.any && c.month.any && c.dow.any && c.minute.values.size > 1) {
    const sorted = [...c.minute.values].sort((a, b) => a - b)
    const step = (sorted[1] ?? 0) - (sorted[0] ?? 0)
    if (sorted.every((v, i) => v === (sorted[0] ?? 0) + i * step) && sorted[0] === 0) return `Every ${step} minutes`
  }
  return expr
}
