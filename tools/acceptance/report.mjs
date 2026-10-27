// Prints the checks of every scenario under an evidence folder as Markdown, for docs/ACCEPTANCE-0.0.1b.md.
//   node report.mjs [evidence-dir]
import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'

const dir = process.argv[2] ?? 'D:\\verne-build\\ogun\\evidence'
const order = ['s1', 's2', 's2b', 's3', 's4', 's5', 's6', 's7', 's8', 's9', 's10']
let out = ''
for (const s of order) {
  const f = join(dir, s, 'results.json')
  if (!existsSync(f)) {
    out += `### ${s}\n\nNot run.\n\n`
    continue
  }
  const rows = JSON.parse(readFileSync(f, 'utf8'))
  const ok = rows.filter((r) => r.ok).length
  out += `### ${s}: ${ok} of ${rows.length} checks passed\n\n| Check | Result |\n|---|---|\n`
  for (const r of rows) out += `| ${r.name.replace(/\|/g, '/')} | ${r.ok ? 'pass' : 'FAIL'} |\n`
  const shots = existsSync(join(dir, s)) ? readdirSync(join(dir, s)).filter((n) => n.endsWith('.png')) : []
  if (shots.length) out += `\nScreenshots: ${shots.join(', ')}\n`
  out += '\n'
}
process.stdout.write(out)
