import { useMemo, useState } from 'preact/hooks'

import type { ArtifactView, LineageResult } from '../api/types'
import { parseCsv } from '../lib/csv'
import { asText, decodeBase64, fmtBytes, fmtTime } from '../lib/format'
import { href, navigate, useAsync, useBackend } from '../lib/hooks'
import { Empty, ErrorBox, Hash, JsonBlock, Kv, Loading, Notice, PageHead, Tabs } from '../ui/components'

/** Artifacts up to this size are previewed inline. */
const PREVIEW_BYTES = 256 * 1024

export function Artifacts({ runId, blake3 }: { runId?: string; blake3?: string }) {
  const backend = useBackend()
  const [input, setInput] = useState(runId ?? '')
  const list = useAsync(
    () => (runId ? backend.artifactsList(runId) : Promise.resolve(null)),
    [backend, runId],
  )
  const [tab, setTab] = useState<'artifacts' | 'lineage'>('artifacts')
  return (
    <>
      <PageHead title="Artifacts" />
      <form
        class="form-row"
        onSubmit={(e) => {
          e.preventDefault()
          navigate(href('artifacts', { run: input.trim() }))
        }}
      >
        <label class="field">
          Run
          <input value={input} placeholder="run id" size={34} onInput={(e) => setInput((e.target as HTMLInputElement).value)} />
        </label>
        <button type="submit" class="btn">
          Show
        </button>
        {runId ? (
          <a class="btn" href={href('runs', runId)}>
            Open the run
          </a>
        ) : null}
      </form>
      {!runId ? <Empty>Artifacts are listed per run. Open a run, or enter its id.</Empty> : null}
      {runId ? (
        <>
          <Tabs
            tabs={[
              ['artifacts', 'Artifacts'],
              ['lineage', 'Run lineage'],
            ]}
            value={tab}
            onChange={setTab}
          />
          {tab === 'artifacts' ? (
            <>
              <ErrorBox error={list.error} />
              {list.loading && !list.data ? <Loading /> : null}
              {list.data && list.data.artifacts.length === 0 ? <Empty>This run has committed no artifacts yet.</Empty> : null}
              {list.data && list.data.artifacts.length > 0 ? (
                <ArtifactTable artifacts={list.data.artifacts} runId={runId} selected={blake3} />
              ) : null}
              {blake3 ? <ArtifactDetail blake3={blake3} /> : null}
            </>
          ) : (
            <Lineage q={{ run_id: runId }} />
          )}
        </>
      ) : null}
    </>
  )
}

function ArtifactTable({ artifacts, runId, selected }: { artifacts: ArtifactView[]; runId: string; selected?: string | undefined }) {
  return (
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Node</th>
            <th>Output</th>
            <th>Partition</th>
            <th>Media type</th>
            <th class="num">Size</th>
            <th>BLAKE3</th>
            <th class="hide-narrow">Committed</th>
          </tr>
        </thead>
        <tbody>
          {artifacts.map((a) => (
            <tr aria-selected={a.blake3 === selected}>
              <td class="mono">{a.node}</td>
              <td class="mono">{a.output}</td>
              <td class="mono">{a.partition || '–'}</td>
              <td class="mono">{a.media_type}</td>
              <td class="num">{fmtBytes(a.size_bytes)}</td>
              <td>
                <a href={href('artifacts', { run: runId, blake3: a.blake3 })}>
                  <Hash value={a.blake3} short />
                </a>
              </td>
              <td class="hide-narrow">{fmtTime(a.committed_at)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function Preview({ media, bytes }: { media: string; bytes: Uint8Array }) {
  const text = useMemo(() => asText(bytes), [bytes])
  if (text === null) return <Notice>Binary content ({fmtBytes(bytes.length)}); no preview.</Notice>
  const base = media.split(';')[0]?.trim() ?? ''
  if (base === 'application/json' || base.endsWith('+json')) {
    try {
      return <JsonBlock value={JSON.parse(text)} />
    } catch {
      // Not valid JSON after all: show the text.
    }
  }
  if (base === 'text/csv' || base === 'text/tab-separated-values') {
    const rows = parseCsv(text, 200, base === 'text/csv' ? ',' : '\t')
    const [head, ...body] = rows
    return (
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              {(head ?? []).map((h) => (
                <th>{h}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {body.map((r) => (
              <tr>
                {r.map((c) => (
                  <td class="mono">{c}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    )
  }
  // Text content is data: rendered as text, never as markup.
  return <pre class="code-block mono">{text.length > 200_000 ? text.slice(0, 200_000) + '\n…' : text}</pre>
}

function ArtifactDetail({ blake3 }: { blake3: string }) {
  const backend = useBackend()
  const art = useAsync(() => backend.artifactsGet(blake3, PREVIEW_BYTES), [backend, blake3])
  const bytes = useMemo(
    () => (art.data?.content_base64 ? decodeBase64(art.data.content_base64) : null),
    [art.data],
  )
  const [tab, setTab] = useState<'preview' | 'lineage'>('preview')
  const a = art.data?.artifact
  return (
    <div class="panel" style={{ marginTop: '16px' }}>
      <h2>Artifact</h2>
      <ErrorBox error={art.error} />
      {art.loading && !a ? <Loading /> : null}
      {a ? (
        <>
          <Kv
            rows={[
              ['BLAKE3', <Hash value={a.blake3} />],
              ['artifact id', <span class="mono">{a.artifact_id}</span>],
              ['media type', <span class="mono">{a.media_type}</span>],
              ['size', `${fmtBytes(a.size_bytes)} (${a.size_bytes} bytes)`],
              ['produced by', <span class="mono">{`${a.node}.${a.output}${a.partition ? ` [${a.partition}]` : ''}`}</span>],
              ['run', <a class="mono" href={href('runs', a.run_id)}>{a.run_id}</a>],
              ['committed', fmtTime(a.committed_at)],
            ]}
          />
          <Tabs
            tabs={[
              ['preview', 'Preview'],
              ['lineage', 'Lineage'],
            ]}
            value={tab}
            onChange={setTab}
          />
          {tab === 'preview' ? (
            bytes ? (
              <Preview media={a.media_type} bytes={bytes} />
            ) : (
              <Notice>
                {a.size_bytes > PREVIEW_BYTES
                  ? `Larger than ${fmtBytes(PREVIEW_BYTES)}; no inline preview.`
                  : 'No preview available.'}
              </Notice>
            )
          ) : (
            <Lineage q={{ blake3: a.blake3 }} />
          )}
        </>
      ) : null}
    </div>
  )
}

interface OlDataset {
  namespace?: string
  name?: string
}

function datasets(v: unknown): string {
  if (!Array.isArray(v) || !v.length) return '–'
  return (v as OlDataset[]).map((d) => `${d.namespace ? d.namespace + ':' : ''}${d.name ?? '?'}`).join(', ')
}

/** OpenLineage run events (O-05), as a table with the raw events beside it. */
export function Lineage({ q }: { q: { run_id?: string; blake3?: string } }) {
  const backend = useBackend()
  const lin = useAsync<LineageResult>(() => backend.lineageGet(q), [backend, q.run_id, q.blake3])
  const [raw, setRaw] = useState(false)
  if (lin.error) return <ErrorBox error={lin.error} />
  if (lin.loading && !lin.data) return <Loading />
  const events = lin.data?.events ?? []
  if (!events.length) return <Empty>No lineage events.</Empty>
  return (
    <>
      <div class="toolbar">
        <label class="check">
          <input type="checkbox" checked={raw} onChange={(e) => setRaw((e.target as HTMLInputElement).checked)} />
          show the OpenLineage events
        </label>
      </div>
      {raw ? (
        <JsonBlock value={events} />
      ) : (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Time</th>
                <th>Event</th>
                <th>Job</th>
                <th>Inputs</th>
                <th>Outputs</th>
              </tr>
            </thead>
            <tbody>
              {events.map((e) => {
                const job = e.job as { namespace?: string; name?: string } | undefined
                return (
                  <tr>
                    <td>{fmtTime(e.eventTime as string | undefined)}</td>
                    <td class="mono">{String(e.eventType ?? '–')}</td>
                    <td class="mono">{job ? `${job.namespace ?? ''}:${job.name ?? ''}` : '–'}</td>
                    <td class="mono wrap">{datasets(e.inputs)}</td>
                    <td class="mono wrap">{datasets(e.outputs)}</td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
    </>
  )
}
