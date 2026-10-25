// Compares lib/cron.ts with the scheduler: for each cron in the example workflows, `ogun next` prints the next
// fire time; the TypeScript helper must print the same one from the same clock.
//
//   node apps/studio/web/scripts/cron-check.mjs [path-to-ogun-binary]

import { execFileSync } from 'node:child_process'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repo = resolve(here, '../../../..')
const bin = process.argv[2] ?? [join(repo, 'target/debug/ogun.exe'), join(repo, 'target/debug/ogun')].find(existsSync)
if (!bin) {
  console.error('build the CLI first: cargo build -p ogun-cli')
  process.exit(2)
}

// Strip the types of lib/cron.ts with the TypeScript compiler that is already a dev dependency.
const ts = (await import(pathToFileURL(join(here, '../node_modules/typescript/lib/typescript.js')).href)).default
const source = readFileSync(join(here, '../src/lib/cron.ts'), 'utf8')
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText
const { nextFire, describeCron } = await import('data:text/javascript;base64,' + Buffer.from(js).toString('base64'))

const files = []
for (const dir of ['examples', 'examples/smartair', 'examples/bikeshare']) {
  for (const f of readdirSync(join(repo, dir))) if (f.endsWith('.workflow.json')) files.push(join(repo, dir, f))
}
let checked = 0
let failed = 0
for (const file of files) {
  const doc = JSON.parse(readFileSync(file, 'utf8'))
  for (const t of doc.triggers ?? []) {
    if (t.kind !== 'cron') continue
    const printed = execFileSync(bin, ['next', file], { encoding: 'utf8' }).split(/\r?\n/).find((l) => /^\d{4}-\d\d-\d\dT/.test(l.trim()))
    const mine = nextFire(t.cron, new Date())?.toISOString()
    checked++
    const ok = printed?.trim() === mine
    if (!ok) failed++
    console.log(`${ok ? 'ok  ' : 'FAIL'} ${doc.workflow.padEnd(20)} ${t.cron.padEnd(14)} cli=${printed?.trim()} ts=${mine}  (${describeCron(t.cron)})`)
  }
}
console.log(`${checked - failed}/${checked} agree with ogun next`)
process.exit(failed ? 1 : 0)
