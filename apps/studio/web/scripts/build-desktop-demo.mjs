// Builds the desktop app on the scripted backend, no daemon needed:
//
//   node scripts/build-desktop-demo.mjs
//
// It builds the frontend in desktop-demo mode, then compiles the Tauri shell with a config override (the scripted
// frontend) and copies the executable to <repo>/dist/desktop/. The override is a build-time
// setting, so the normal `cargo build` of the shell is unchanged. Windows needs the WebView2 runtime, which
// Windows 11 ships with. Nothing is committed or pushed here.

import { spawnSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const web = resolve(here, '..')
const shell = resolve(web, '../src-tauri')
const repo = resolve(web, '../../..')
const win = process.platform === 'win32'

function run(cmd, args, opts) {
  const r = spawnSync(cmd, args, { stdio: 'inherit', ...opts })
  if (r.status !== 0) {
    console.error(`${cmd} ${args.join(' ')} failed (${r.status ?? r.signal})`)
    process.exit(r.status ?? 1)
  }
}

run(win ? 'npm.cmd' : 'npm', ['run', 'build:desktop-demo'], { cwd: web, shell: win })

// The window configuration is an array, and a config override replaces an array whole, so the override starts from
// the window the shell would have on this platform (tauri.conf.json plus tauri.<platform>.conf.json) and changes
// only the title.
const readJson = (f) => (existsSync(f) ? JSON.parse(readFileSync(f, 'utf8')) : {})
const platformName = { win32: 'windows', darwin: 'macos', linux: 'linux' }[process.platform]
const baseWindow = readJson(join(shell, 'tauri.conf.json')).app?.windows?.[0] ?? {}
const platformWindow = readJson(join(shell, `tauri.${platformName}.conf.json`)).app?.windows?.[0] ?? {}
const override = {
  productName: 'Ogun Studio',
  build: { frontendDist: '../web/dist-desktop-demo' },
  app: { windows: [{ ...baseWindow, ...platformWindow, title: 'Ogun Studio' }] },
}
run('cargo', ['build', '--release'], { cwd: shell, env: { ...process.env, TAURI_CONFIG: JSON.stringify(override) } })

const exe = join(shell, 'target', 'release', win ? 'ogun-studio.exe' : 'ogun-studio')
if (!existsSync(exe)) {
  console.error(`expected ${exe}`)
  process.exit(1)
}
const out = join(repo, 'dist', 'desktop')
mkdirSync(out, { recursive: true })
const dest = join(out, win ? 'ogun-studio.exe' : 'ogun-studio')
copyFileSync(exe, dest)
console.log(`\nbuilt ${dest}`)
