import { fmtAgo, fmtTime } from '../lib/format'
import { useAsync, useBackend } from '../lib/hooks'
import { Empty, ErrorBox, Hash, Loading, PageHead } from '../ui/components'

export function Edges() {
  const backend = useBackend()
  const list = useAsync(() => backend.edgesList(), [backend])
  return (
    <>
      <PageHead title="Edges" />
      <p class="muted">
        Edges enrolled on this hub (O-11). A person enrols an edge's Ed25519 key on the hub's terminal; the hub then
        accepts sealed windows only from that key, for partitions under the edge's own namespace.
      </p>
      <ErrorBox error={list.error} />
      {list.loading && !list.data ? <Loading /> : null}
      {list.data && list.data.edges.length === 0 ? <Empty>No edges enrolled.</Empty> : null}
      {list.data && list.data.edges.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Edge id</th>
                <th>Public key</th>
                <th>Enrolled</th>
                <th>Last sync</th>
              </tr>
            </thead>
            <tbody>
              {list.data.edges.map((e) => (
                <tr>
                  <td>{e.name}</td>
                  <td class="mono">{e.edge_id}</td>
                  <td>
                    <Hash value={e.public_key} short />
                  </td>
                  <td>{fmtTime(e.enrolled_at)}</td>
                  <td title={fmtTime(e.last_sync_at)}>{e.last_sync_at ? fmtAgo(e.last_sync_at) : 'never'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </>
  )
}
