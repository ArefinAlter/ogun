// Windows only: assembles the portable zip of Ogun Studio from a release build, in the same layout the installer
// produces next to ogun-studio.exe: the sidecars without their target triple, the bundled sample projects under
// samples/ and the embeddable Python under python/.
//
//   node scripts/portable.mjs <out-dir> [--exe path/to/ogun-studio.exe]
//
// `CARGO_TARGET_DIR` is honoured when --exe is not given. Run after `stage-sidecars.mjs --release`,
// `stage-python.mjs` and a release build of the shell.

import { cpSync, existsSync, mkdirSync, readdirSync, rmSync, copyFileSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repo = resolve(here, '..', '..', '..')
const bin = resolve(here, '..', 'src-tauri', 'binaries')
const outDir = resolve(process.argv[2] ?? '.')
const i = process.argv.indexOf('--exe')
const target = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(repo, 'apps', 'studio', 'src-tauri', 'target')
const exe = i >= 0 ? resolve(process.argv[i + 1]) : join(target, 'release', 'ogun-studio.exe')
const version = process.env.OGUN_VERSION ?? '0.0.1b'
if (!existsSync(exe)) throw new Error(`${exe} does not exist; build the shell first`)

const name = `ogun-studio-${version}-windows-x64-portable`
const stage = join(outDir, name)
rmSync(stage, { recursive: true, force: true })
mkdirSync(stage, { recursive: true })
copyFileSync(exe, join(stage, 'ogun-studio.exe'))
for (const f of readdirSync(bin)) {
  const m = /^(.*)-x86_64-pc-windows-msvc\.exe$/.exec(f)
  if (m) copyFileSync(join(bin, f), join(stage, `${m[1]}.exe`))
}
cpSync(join(bin, 'python'), join(stage, 'python'), { recursive: true })
for (const s of ['starter', 'bikeshare']) cpSync(join(repo, 'examples', s), join(stage, 'samples', s), { recursive: true })
copyFileSync(join(repo, 'LICENSE'), join(stage, 'LICENSE'))
copyFileSync(join(repo, 'NOTICE'), join(stage, 'NOTICE'))
const zip = join(outDir, `${name}.zip`)
rmSync(zip, { force: true })
execFileSync('powershell.exe', ['-NoProfile', '-Command', `Compress-Archive -Force -Path '${stage}\\*' -DestinationPath '${zip}'`], { stdio: 'inherit' })
console.log(`wrote ${zip}`)
