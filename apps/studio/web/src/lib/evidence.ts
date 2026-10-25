// An evidence bundle for one run: what ran, which exact workflow, what it produced and where each artifact came
// from, in one file a person can keep, send or attach to an audit. It is assembled in this window from what the
// daemon reports; nothing is uploaded. The bundle carries the same facts the run page shows, in the same words:
// workflow hash, artifact digests (BLAKE3) and the OpenLineage events (O-05). It carries no secret and no
// configuration value the run page would not show.

import type { Backend } from '../api'
import type { EvidenceSettings } from '../api/types'
import { asText, decodeBase64 } from './format'

export interface EvidenceBundle {
  evidence_version: 0
  generated_at: string
  generator: string
  run: Record<string, unknown>
  workflow: { name: string; version: number; workflow_hash: string; document: unknown }
  artifacts: { node: string; output: string; partition: string; media_type: string; size_bytes: number; blake3: string; committed_at: string; preview?: string }[]
  lineage?: Record<string, unknown>[]
  notice: string
  /** SHA-256 (hex) of this document serialised without this member. */
  document_sha256?: string
}

const PREVIEW_BYTES = 4096
const INLINE_MAX = 64 * 1024

async function sha256(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text)
  const digest = await crypto.subtle.digest('SHA-256', bytes)
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('')
}

export async function buildEvidence(backend: Backend, runId: string, opts: Pick<EvidenceSettings, 'include_lineage' | 'include_previews'>, appVersion: string): Promise<EvidenceBundle> {
  const run = await backend.runsGet(runId)
  const wf = await backend.workflowsGet(run.workflow, run.version)
  const list = await backend.artifactsList(runId)
  const artifacts: EvidenceBundle['artifacts'] = []
  for (const a of list.artifacts) {
    const row: EvidenceBundle['artifacts'][number] = { node: a.node, output: a.output, partition: a.partition, media_type: a.media_type, size_bytes: a.size_bytes, blake3: a.blake3, committed_at: a.committed_at }
    if (opts.include_previews && a.size_bytes <= INLINE_MAX) {
      try {
        const got = await backend.artifactsGet(a.blake3, INLINE_MAX)
        if (got.content_base64) {
          const text = asText(decodeBase64(got.content_base64))
          if (text !== null) row.preview = text.slice(0, PREVIEW_BYTES)
        }
      } catch {
        // A preview is a convenience; the digest is the evidence.
      }
    }
    artifacts.push(row)
  }
  const { nodes, ...runFacts } = run
  const bundle: EvidenceBundle = {
    evidence_version: 0,
    generated_at: new Date().toISOString(),
    generator: `Ogun Studio ${appVersion}`,
    run: { ...runFacts, node_instances: nodes?.length ?? 0, states: countStates(nodes ?? []) },
    workflow: { name: wf.summary.name, version: wf.version, workflow_hash: wf.workflow_hash, document: wf.workflow },
    artifacts,
    notice: 'Digests are BLAKE3 of the artifact bytes. The workflow hash is the BLAKE3 of the workflow document in normal form (O-02). This file lists what the daemon reported when it was made.',
  }
  if (opts.include_lineage) bundle.lineage = (await backend.lineageGet({ run_id: runId })).events
  bundle.document_sha256 = await sha256(JSON.stringify(bundle))
  return bundle
}

function countStates(nodes: { state: string }[]): Record<string, number> {
  const out: Record<string, number> = {}
  for (const n of nodes) out[n.state] = (out[n.state] ?? 0) + 1
  return out
}

const esc = (v: unknown) => String(v).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c] ?? c)

/** A self-contained, script-free HTML rendering of the bundle for printing or filing. */
export function evidenceHtml(b: EvidenceBundle): string {
  const run = b.run as { run_id?: string; state?: string; created_at?: string; partition_count?: number; states?: Record<string, number> }
  const rows = b.artifacts.map((a) => `<tr><td>${esc(a.node)}.${esc(a.output)}</td><td>${esc(a.partition || '-')}</td><td>${esc(a.media_type)}</td><td class=n>${esc(a.size_bytes)}</td><td class=m>${esc(a.blake3)}</td></tr>`).join('')
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Evidence for ${esc(run.run_id)}</title>
<style>body{font:14px/1.5 system-ui,sans-serif;max-width:960px;margin:32px auto;padding:0 16px;color:#222}h1{font-size:20px}h2{font-size:15px;margin-top:28px}dl{display:grid;grid-template-columns:max-content 1fr;gap:4px 16px}dt{color:#666}table{border-collapse:collapse;width:100%;font-size:12px}td,th{border-bottom:1px solid #ddd;padding:5px 8px;text-align:left;vertical-align:top}.m{font:11px ui-monospace,monospace;word-break:break-all}.n{text-align:right}code{font:12px ui-monospace,monospace}</style></head><body>
<h1>Evidence for run <code>${esc(run.run_id)}</code></h1>
<dl><dt>Workflow</dt><dd>${esc(b.workflow.name)} v${esc(b.workflow.version)}</dd><dt>Workflow hash</dt><dd class=m>${esc(b.workflow.workflow_hash)}</dd><dt>State</dt><dd>${esc(run.state)}</dd><dt>Created</dt><dd>${esc(run.created_at)}</dd><dt>Partitions</dt><dd>${esc(run.partition_count)}</dd><dt>Node instances</dt><dd>${esc(JSON.stringify(run.states ?? {}))}</dd><dt>Generated</dt><dd>${esc(b.generated_at)} by ${esc(b.generator)}</dd></dl>
<h2>Artifacts (${b.artifacts.length})</h2><table><thead><tr><th>Output</th><th>Partition</th><th>Media type</th><th class=n>Bytes</th><th>BLAKE3</th></tr></thead><tbody>${rows}</tbody></table>
${b.lineage ? `<h2>Lineage</h2><p>${b.lineage.length} OpenLineage events are in the JSON form of this bundle.</p>` : ''}
<p>${esc(b.notice)}</p><p class=m>document sha256 ${esc(b.document_sha256 ?? '')}</p></body></html>`
}

/** Saves text as a file through the browser. Returns false when the shell does not allow downloads. */
export function download(name: string, text: string, type: string): boolean {
  try {
    const url = URL.createObjectURL(new Blob([text], { type }))
    const a = document.createElement('a')
    a.href = url
    a.download = name
    document.body.appendChild(a)
    a.click()
    a.remove()
    setTimeout(() => URL.revokeObjectURL(url), 10_000)
    return true
  } catch {
    return false
  }
}
