// Builds the Ogun binaries that ship inside Studio and copies them where Tauri's `externalBin` expects them:
// `src-tauri/binaries/<name>-<target triple>[.exe]`. Tauri then places them next to `ogun-studio` in the
// installer, which is where `ogund` pins the Studio and CLI executables it trusts (O-15 section 4.4).
//
//   node scripts/stage-sidecars.mjs [--release]
//
// `CARGO_TARGET_DIR` is honoured, so the build cache can live on another drive.

import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repo = resolve(here, '..', '..', '..')
const out = resolve(here, '..', 'src-tauri', 'binaries')
const release = process.argv.includes('--release')
const windows = process.platform === 'win32'
const ext = windows ? '.exe' : ''

const triple = execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
  .split('\n')
  .find((l) => l.startsWith('host:'))
  ?.slice(5)
  .trim()
if (!triple) throw new Error('cannot read the host target triple from rustc -vV')

// binary name -> the package that builds it
const bins = {
  ogund: 'ogun-daemon',
  ogun: 'ogun-cli',
  'ogun-op-llm': 'ogun-op-llm',
  'ogun-op-shim': 'ogun-op-shims',
  'ogun-ingest': 'ogun-ingest',
  'ogun-bench': 'ogun-bench',
}
if (windows) bins['ogun-sandbox'] = 'ogun-sandbox'

const args = ['build', ...(release ? ['--release'] : [])]
for (const pkg of new Set(Object.values(bins))) args.push('-p', pkg)
console.log(`cargo ${args.join(' ')}`)
execFileSync('cargo', args, { cwd: repo, stdio: 'inherit', env: { ...process.env, CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '4' } })

const target = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(repo, 'target')
const profile = release ? 'release' : 'debug'
mkdirSync(out, { recursive: true })
for (const name of Object.keys(bins)) {
  const from = join(target, profile, `${name}${ext}`)
  if (!existsSync(from)) throw new Error(`${from} was not built`)
  const to = join(out, `${name}-${triple}${ext}`)
  copyFileSync(from, to)
  console.log(`staged ${name} -> ${to}`)
}
