import type { StreamView } from '../api/types'
import { fmtAgo, fmtCount, fmtMs } from '../lib/format'
import { href, useAsync, useBackend, useLive } from '../lib/hooks'
import { Empty, ErrorBox, Hash, Kv, Loading, PageHead } from '../ui/components'

function watermark(ms: number | undefined): string {
  if (ms === undefined) return 'none yet'
  return `${fmtMs(ms)} (${fmtAgo(new Date(ms).toISOString())})`
}

export function Streams() {
  const backend = useBackend()
  const list = useAsync(() => backend.streamsList(), [backend])
  useLive(() => list.reload())
  return (
    <>
      <PageHead title="Streams" />
      <p class="muted">
        Stream sources of stored workflows: the low watermark across keys, windows open and sealed, records buffered
        but not yet in a sealed window, and records dropped by the stream's full-buffer policy (O-09, O-10).
      </p>
      <ErrorBox error={list.error} />
      {list.loading && !list.data ? <Loading /> : null}
      {list.data && list.data.streams.length === 0 ? <Empty>No stream sources.</Empty> : null}
      {list.data && list.data.streams.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Workflow</th>
                <th>Node</th>
                <th>Operator</th>
                <th>Watermark</th>
                <th class="num">Open</th>
                <th class="num">Sealed</th>
                <th class="num">Backlog</th>
                <th class="num">Dropped</th>
              </tr>
            </thead>
            <tbody>
              {list.data.streams.map((s) => (
                <tr>
                  <td>
                    <a href={href('workflows', s.workflow)}>{s.workflow}</a>
                  </td>
                  <td>
                    <a class="mono" href={href('streams', s.workflow, s.node)}>
                      {s.node}
                    </a>
                  </td>
                  <td class="mono">{s.op}</td>
                  <td class="mono">{s.watermark_ms !== undefined ? fmtMs(s.watermark_ms) : '–'}</td>
                  <td class="num">{fmtCount(s.windows_open)}</td>
                  <td class="num">{fmtCount(s.windows_sealed)}</td>
                  <td class="num">{fmtCount(s.buffered)}</td>
                  <td class="num">{s.dropped ? <span class="state s-failed">{fmtCount(s.dropped)}</span> : '0'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </>
  )
}

export function StreamDetail({ workflow, node }: { workflow: string; node: string }) {
  const backend = useBackend()
  const s = useAsync<StreamView>(() => backend.streamsStatus(workflow, node), [backend, workflow, node])
  useLive(() => s.reload())
  const v = s.data
  return (
    <>
      <PageHead title={`${workflow} · ${node}`}>
        <a class="btn" href={href('streams')}>
          All streams
        </a>
      </PageHead>
      <ErrorBox error={s.error} />
      {s.loading && !v ? <Loading /> : null}
      {v ? (
        <>
          <div class="panel">
            <Kv
              rows={[
                ['operator', <span class="mono">{v.op}</span>],
                ['watermark', watermark(v.watermark_ms)],
                ['windows open', fmtCount(v.windows_open)],
                ['windows sealed', fmtCount(v.windows_sealed)],
                ['backlog', `${fmtCount(v.buffered)} records not yet in a sealed window`],
                ['dropped', fmtCount(v.dropped)],
                ...(v.late !== undefined ? [['late', fmtCount(v.late)] as [string, string]] : []),
                ...(v.invalid !== undefined ? [['invalid', fmtCount(v.invalid)] as [string, string]] : []),
              ]}
            />
          </div>
          {v.open_windows?.length ? (
            <>
              <h2>Open windows</h2>
              <div class="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Window</th>
                      <th class="num">Records</th>
                      <th>Closes</th>
                    </tr>
                  </thead>
                  <tbody>
                    {v.open_windows.map((w) => (
                      <tr>
                        <td class="mono">{w.key}</td>
                        <td class="num">{fmtCount(w.records)}</td>
                        <td class="mono">{w.closes_at_ms !== undefined ? fmtMs(w.closes_at_ms) : '–'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          ) : null}
          {v.sealed_windows?.length ? (
            <>
              <h2>Recently sealed</h2>
              <div class="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Window</th>
                      <th class="num">Revision</th>
                      <th class="num">Records</th>
                      <th>BLAKE3</th>
                    </tr>
                  </thead>
                  <tbody>
                    {v.sealed_windows.map((w) => (
                      <tr>
                        <td class="mono">{w.key}</td>
                        <td class="num">{w.revision}</td>
                        <td class="num">{fmtCount(w.records)}</td>
                        <td>
                          <Hash value={w.blake3} short />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          ) : null}
          <p class="muted">
            Window partitions of this stream are on the <a href={href('partitions', { workflow })}>partitions</a> page.
          </p>
        </>
      ) : null}
    </>
  )
}
