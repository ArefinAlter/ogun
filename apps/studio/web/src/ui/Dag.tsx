// The workflow DAG as SVG, with optional live node-state bars (count of instances per state for each node).

import type { NodeState, WorkflowNodeDoc } from '../api/types'
import { layoutDag } from '../lib/dag'

const BAR_ORDER: NodeState[] = [
  'succeeded',
  'running',
  'ready',
  'retry_scheduled',
  'waiting_for_user',
  'waiting_for_session',
  'failed',
]

function barClass(s: NodeState): string {
  return BAR_ORDER.includes(s) ? `bar-${s}` : 'bar-other'
}

export function Dag({
  nodes,
  states,
  selected,
  onSelect,
}: {
  nodes: WorkflowNodeDoc[]
  /** Per node id: instance counts by state. */
  states?: Map<string, Map<NodeState, number>>
  selected?: string | undefined
  onSelect?: (id: string) => void
}) {
  const lay = layoutDag(nodes)
  return (
    <div class="dag-wrap">
      <svg
        class="dag"
        width={lay.width}
        height={lay.height}
        viewBox={`0 0 ${lay.width} ${lay.height}`}
        role="img"
        aria-label={`Workflow graph with ${nodes.length} nodes and ${lay.edges.length} edges`}
      >
        <defs>
          <marker id="dag-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
            <path d="M0 0 L8 4 L0 8 z" fill="var(--line-strong)" />
          </marker>
        </defs>
        {lay.edges.map((e) => (
          <g>
            <path class={`edge ${e.route ? 'route' : ''}`} d={e.path} marker-end="url(#dag-arrow)" />
            {e.label ? (
              <text class="edge-label" x={e.lx} y={e.ly} text-anchor="middle">
                {e.label}
              </text>
            ) : null}
          </g>
        ))}
        {lay.nodes.map((n) => {
          const counts = states?.get(n.id)
          const total = counts ? [...counts.values()].reduce((a, b) => a + b, 0) : 0
          let x = n.x + 8
          const w = n.w - 16
          return (
            <g
              class={`node ${selected === n.id ? 'sel' : ''}`}
              tabIndex={onSelect ? 0 : -1}
              role={onSelect ? 'button' : undefined}
              aria-label={`node ${n.id}, ${n.op}`}
              onClick={() => onSelect?.(n.id)}
              onKeyDown={(ev) => {
                if (ev.key === 'Enter' || ev.key === ' ') onSelect?.(n.id)
              }}
              style={onSelect ? { cursor: 'pointer' } : undefined}
            >
              <rect x={n.x} y={n.y} width={n.w} height={n.h} rx="4" />
              <text x={n.x + 10} y={n.y + 20}>
                {n.id.length > 22 ? n.id.slice(0, 21) + '…' : n.id}
              </text>
              <text class="op" x={n.x + 10} y={n.y + 36}>
                {n.op.length > 26 ? n.op.slice(0, 25) + '…' : n.op}
              </text>
              {counts && total > 0 ? (
                <g>
                  {[...counts.entries()].map(([s, c]) => {
                    const bw = (c / total) * w
                    const r = <rect class={barClass(s)} x={x} y={n.y + n.h - 12} width={Math.max(bw, 1)} height="5" />
                    x += bw
                    return r
                  })}
                  <title>{[...counts.entries()].map(([s, c]) => `${s}: ${c}`).join(', ')}</title>
                </g>
              ) : null}
            </g>
          )
        })}
      </svg>
    </div>
  )
}
