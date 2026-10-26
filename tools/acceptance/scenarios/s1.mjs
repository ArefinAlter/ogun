// Scenario 1: launch with no command line, the app owns its engine, an empty workspace, the bundled sample imported
// with real hashes, a clean shutdown and a second launch that finds the data again.
import { join } from 'node:path'

export async function run({ ev, exe, root, cleanDir, App, ps, alive, sleep, launch }) {
  cleanDir(root)
  const { app, info } = await launch(root)
  ev.save('engine_info.json', info)
  ev.check('the app owns a running engine', info.state === 'running' && info.pid > 0, JSON.stringify(info))
  const engines = app.engines()
  ev.check('exactly one ogund child of the app', engines.length === 1, JSON.stringify(engines.map((e) => e.pid)))
  const eng = engines[0]
  ev.check('the engine takes no secret on its command line', eng && !/token|secret|credential/i.test(eng.cmd), eng?.cmd)
  const listening = ps(
    `Get-NetTCPConnection -State Listen -OwningProcess ${eng.pid} -ErrorAction SilentlyContinue | Measure-Object | Select-Object -ExpandProperty Count`,
  )
  ev.check('the engine listens on no TCP port (a named pipe only)', listening === '0', `listening sockets: ${listening}`)
  const studio = await app.invoke('studio_info')
  ev.save('studio_info.json', studio)
  ev.check('the link is the local engine and it answers', studio.link === 'local' && studio.connected, JSON.stringify(studio).slice(0, 200))
  const wfs = await app.invoke('workflows_list')
  ev.check('the workspace starts empty', (wfs.workflows ?? []).length === 0, JSON.stringify(wfs).slice(0, 200))
  await app.shot(join(ev.dir, '01-empty-workspace.png'))
  const samples = await app.invoke('samples_list')
  ev.save('samples.json', samples)
  ev.check('the bundled samples are offered', samples.length >= 2, samples.map((s) => s.id).join(','))
  const imported = await app.invoke('sample_import', { id: 'bikeshare' })
  ev.save('sample_import.json', imported)
  const hashes = imported.workflows.map((w) => w.workflow_hash)
  ev.check('the sample import stores workflows with BLAKE3 workflow hashes', imported.workflows.length >= 4 && hashes.every((h) => /^[0-9a-f]{64}$/.test(h)), `${imported.workflows.length} workflows`)
  const again = await app.invoke('sample_import', { id: 'bikeshare' })
  ev.check('importing again creates nothing new', again.workflows.every((w) => w.created === false), JSON.stringify(again.workflows.map((w) => w.created)))
  const listed = await app.invoke('workflows_list')
  const names = new Set(imported.workflows.map((w) => w.name))
  ev.check('workflows.list shows each of them by name', (listed.workflows ?? []).length === names.size, `${(listed.workflows ?? []).length} names, ${imported.workflows.length} versions`)
  await app.goto('#/workflows')
  await app.shot(join(ev.dir, '02-after-sample-import.png'))
  // Quit the app the way a person does and make sure the engine goes with it; then launch again and find the data.
  const shellPid = app.pid
  await app.stop()
  await sleep(3000)
  ev.check('closing the app stops the engine', !alive(eng.pid), `engine pid ${eng.pid}`)
  ev.check('and the app itself', !alive(shellPid))
  const app2 = new App({ root, exe })
  await app2.start()
  const info2 = await app2.waitEngine()
  ev.check('a second launch starts a new engine', info2.pid !== info.pid)
  const wfs2 = await app2.invoke('workflows_list')
  ev.check('the workspace survived the restart', (wfs2.workflows ?? []).length === names.size, `${(wfs2.workflows ?? []).length} workflows`)
  await app2.shot(join(ev.dir, '03-second-launch.png'))
  await app2.stop()
  return ev.finish()
}
