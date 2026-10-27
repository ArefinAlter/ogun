// A scratch probe: starts the app on the existing scratch root (nothing is cleaned) and writes the state of its runs.
import { writeFileSync } from 'node:fs'
import { join } from 'node:path'

export async function run({ ev, root, launch, sleep }) {
  const { app } = await launch(root)
  await sleep(3000)
  const runs = (await app.invoke('runs_list', { params: { limit: 20 } })).runs ?? []
  const out = { runs: [] }
  for (const r of runs) {
    const full = await app.invoke('runs_get', { params: { run_id: r.run_id } })
    const byState = {}
    for (const n of full.nodes ?? []) byState[n.state] = (byState[n.state] ?? 0) + 1
    out.runs.push({ run_id: r.run_id, workflow: r.workflow, state: full.state, partitions: full.partition_count, nodes: byState, sample: (full.nodes ?? []).filter((n) => !['succeeded', 'cancelled'].includes(n.state)).slice(0, 6) })
  }
  out.partitions = await app.invoke('partitions_status', { params: { workflow: 'acc-daily' } })
  writeFileSync(join(ev.dir, 'inspect.json'), JSON.stringify(out, null, 2))
  ev.check('inspected', true)
  await app.stop(true)
  return ev.finish()
}
