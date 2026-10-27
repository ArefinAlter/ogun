// A scratch probe for exploring API shapes against the real app (not part of the acceptance run).
import { writeFileSync } from 'node:fs'
import { join } from 'node:path'

export async function run({ ev, root, cleanDir, launch, key }) {
  cleanDir(root)
  const { app } = await launch(root)
  const out = {}
  const calls = [
    ['runtime.get', {}],
    ['runtime.set', { patch: { concurrency: 8 }, idempotency_key: key() }],
    ['schedules.list', {}],
    ['sandbox.status', {}],
    ['environments.list', {}],
  ]
  for (const [m, p] of calls) {
    try {
      out[m] = await app.engine(m, p)
    } catch (e) {
      out[m] = { error: String(e.message ?? e), envelope: e.envelope }
    }
  }
  writeFileSync(join(ev.dir, 'probe.json'), JSON.stringify(out, null, 2))
  ev.check('probe ran', true)
  await app.stop(true)
  return ev.finish()
}
