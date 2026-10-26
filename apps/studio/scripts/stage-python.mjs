// Windows only: fetches the embeddable Python that Studio ships for code operators (`python` and `polars`) and
// unpacks it to `src-tauri/binaries/python`, which `tauri.windows.conf.json` bundles as `python/` beside the app.
// The distribution is the one python.org publishes for embedding (PSF licence, the licence file is inside); the
// download is checked against the SHA-256 recorded here. It is stdlib only: an operator that needs a package
// runs with a Python the person installs for their own user.
//
//   node scripts/stage-python.mjs

import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const VERSION = '3.13.9'
const SHA256 = '91d828c2da3a029b41699e918674a0cb379c02cf20dab9c501306885f837402a'
const URL = `https://www.python.org/ftp/python/${VERSION}/python-${VERSION}-embed-amd64.zip`

const here = dirname(fileURLToPath(import.meta.url))
const out = resolve(here, '..', 'src-tauri', 'binaries', 'python')
const cache = process.env.OGUN_DOWNLOAD_DIR ?? resolve(here, '..', 'src-tauri', 'binaries')
if (existsSync(join(out, 'python.exe'))) {
  console.log(`python ${VERSION} is already staged`)
  process.exit(0)
}
mkdirSync(cache, { recursive: true })
const zip = join(cache, `python-${VERSION}-embed-amd64.zip`)
if (!existsSync(zip)) {
  const res = await fetch(URL)
  if (!res.ok) throw new Error(`${URL} answered ${res.status}`)
  writeFileSync(zip, Buffer.from(await res.arrayBuffer()))
}
const sum = createHash('sha256').update(readFileSync(zip)).digest('hex')
if (sum !== SHA256) {
  rmSync(zip)
  throw new Error(`the download's SHA-256 is ${sum}, expected ${SHA256}`)
}
rmSync(out, { recursive: true, force: true })
mkdirSync(out, { recursive: true })
execFileSync('powershell', ['-NoProfile', '-Command', `Expand-Archive -LiteralPath '${zip}' -DestinationPath '${out}' -Force`], { stdio: 'inherit' })
console.log(`staged python ${VERSION} -> ${out}`)
