// Range-compressed partition states (O-15: `{from, to, count, state}`) as a proportional heat strip, and as a
// calendar of cells when the ranges are few enough to expand.

import type { PartitionRange } from '../api/types'
import { fmtCount } from '../lib/format'

const MAX_CELLS = 1500

export function Legend() {
  return (
    <div class="legend" aria-hidden="true">
      <span>
        <i class="p-done" />
        done
      </span>
      <span>
        <i class="p-running" />
        running
      </span>
      <span>
        <i class="p-failed" />
        failed
      </span>
      <span>
        <i class="p-missing" />
        missing
      </span>
    </div>
  )
}

export function Strip({ ranges }: { ranges: PartitionRange[] }) {
  const total = ranges.reduce((a, r) => a + r.count, 0)
  if (!total) return <div class="strip" aria-label="no partitions" />
  return (
    <div
      class="strip"
      role="img"
      aria-label={ranges.map((r) => `${r.state} ${r.count} (${r.from} to ${r.to})`).join('; ')}
    >
      {ranges.map((r) => (
        <span
          class={`p-${r.state}`}
          style={{ flex: `${r.count} 0 0` }}
          title={`${r.state}: ${fmtCount(r.count)} · ${r.from} … ${r.to}`}
        />
      ))}
    </div>
  )
}

/** One cell per partition when there are at most 1 500; otherwise null (the strip is enough). */
export function Calendar({ ranges }: { ranges: PartitionRange[] }) {
  const total = ranges.reduce((a, r) => a + r.count, 0)
  if (!total || total > MAX_CELLS) return null
  const cells: { state: string; title: string }[] = []
  for (const r of ranges) {
    for (let i = 0; i < r.count; i++) {
      const title = r.count === 1 ? r.from : i === 0 ? r.from : i === r.count - 1 ? r.to : `${r.from} … ${r.to}`
      cells.push({ state: r.state, title: `${r.state} · ${title}` })
    }
  }
  return (
    <div class="cal" aria-hidden="true">
      {cells.map((c) => (
        <span class={`p-${c.state}`} title={c.title} />
      ))}
    </div>
  )
}
