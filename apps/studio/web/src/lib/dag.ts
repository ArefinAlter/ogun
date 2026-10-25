// Layered layout of a workflow DAG (O-02 section 5): edges from `inputs` (producer to consumer) and `route`
// (router to target). Columns are longest-path layers from the sources; rows are ordered by the barycentre of
// each node's producers, which keeps most edges short and uncrossed for the small graphs workflows are.

import type { WorkflowNodeDoc } from '../api/types'

export interface LaidNode {
  id: string
  op: string
  x: number
  y: number
  w: number
  h: number
  layer: number
}

export interface LaidEdge {
  from: string
  to: string
  label: string
  route: boolean
  path: string
  lx: number
  ly: number
}

export interface Layout {
  nodes: LaidNode[]
  edges: LaidEdge[]
  width: number
  height: number
}

export const NODE_W = 188
export const NODE_H = 58
const COL_GAP = 84
const ROW_GAP = 22
const PAD = 16

export function layoutDag(nodes: WorkflowNodeDoc[]): Layout {
  const ids = new Set(nodes.map((n) => n.id))
  const raw: { from: string; to: string; label: string; route: boolean }[] = []
  for (const n of nodes) {
    for (const input of n.inputs ?? []) {
      const dot = input.indexOf('.')
      const from = dot > 0 ? input.slice(0, dot) : input
      const port = dot > 0 ? input.slice(dot + 1) : ''
      if (ids.has(from)) raw.push({ from, to: n.id, label: port, route: false })
    }
    for (const r of n.route ?? []) {
      if (ids.has(r.to)) raw.push({ from: n.id, to: r.to, label: 'route', route: true })
    }
  }

  // Longest-path layering (Kahn); a node left over by a cycle (an invalid document) stays in layer 0.
  const preds = new Map<string, string[]>()
  const succs = new Map<string, string[]>()
  for (const id of ids) {
    preds.set(id, [])
    succs.set(id, [])
  }
  for (const e of raw) {
    preds.get(e.to)?.push(e.from)
    succs.get(e.from)?.push(e.to)
  }
  const layer = new Map<string, number>()
  const indeg = new Map<string, number>()
  for (const id of ids) indeg.set(id, preds.get(id)?.length ?? 0)
  const queue = [...ids].filter((id) => indeg.get(id) === 0)
  for (const id of queue) layer.set(id, 0)
  while (queue.length) {
    const id = queue.shift() as string
    for (const s of succs.get(id) ?? []) {
      layer.set(s, Math.max(layer.get(s) ?? 0, (layer.get(id) ?? 0) + 1))
      const d = (indeg.get(s) ?? 0) - 1
      indeg.set(s, d)
      if (d === 0) queue.push(s)
    }
  }
  for (const id of ids) if (!layer.has(id)) layer.set(id, 0)

  const columns: string[][] = []
  for (const n of nodes) {
    const l = layer.get(n.id) ?? 0
    ;(columns[l] ??= []).push(n.id)
  }
  const row = new Map<string, number>()
  columns.forEach((col, ci) => {
    if (ci > 0) {
      const bary = (id: string) => {
        const ps = preds.get(id) ?? []
        if (!ps.length) return Number.MAX_SAFE_INTEGER
        return ps.reduce((a, p) => a + (row.get(p) ?? 0), 0) / ps.length
      }
      col.sort((a, b) => bary(a) - bary(b))
    }
    col.forEach((id, ri) => row.set(id, ri))
  })

  const tallest = Math.max(1, ...columns.map((c) => c.length))
  const colHeight = (n: number) => n * NODE_H + (n - 1) * ROW_GAP
  const height = PAD * 2 + colHeight(tallest)
  const byId = new Map(nodes.map((n) => [n.id, n]))
  const laid: LaidNode[] = []
  columns.forEach((col, ci) => {
    const offset = (colHeight(tallest) - colHeight(col.length)) / 2
    col.forEach((id, ri) => {
      laid.push({
        id,
        op: byId.get(id)?.op ?? '',
        layer: ci,
        x: PAD + ci * (NODE_W + COL_GAP),
        y: PAD + offset + ri * (NODE_H + ROW_GAP),
        w: NODE_W,
        h: NODE_H,
      })
    })
  })
  const pos = new Map(laid.map((n) => [n.id, n]))
  const edges: LaidEdge[] = raw.map((e) => {
    const a = pos.get(e.from) as LaidNode
    const b = pos.get(e.to) as LaidNode
    const x1 = a.x + a.w
    const y1 = a.y + a.h / 2
    const x2 = b.x
    const y2 = b.y + b.h / 2
    const back = x2 <= x1
    const dx = back ? 60 : Math.max(30, (x2 - x1) / 2)
    const path = `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`
    return { ...e, path, lx: (x1 + x2) / 2, ly: (y1 + y2) / 2 - 5 }
  })
  const width = PAD * 2 + Math.max(1, columns.length) * NODE_W + Math.max(0, columns.length - 1) * COL_GAP
  return { nodes: laid, edges, width, height }
}
