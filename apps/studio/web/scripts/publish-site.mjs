// Copies the web build into the ogun-site repository, where Vercel served it at /studio/. Since release 0.0.1a ogun-site
// redirects /studio/ to the hosted hub instead and keeps no copy, so this script is only needed to publish a static
// build somewhere else:
//
//   npm run build:site && node scripts/publish-site.mjs ../../../../ogun-site
//
// The site is static and has no install step, so the build goes in as files under public/studio/. The copy is
// marked noindex (it is a private link, not a page to find by search), and it keeps no browser storage, because the site
// promises none (see VITE_OGUN_NO_STORAGE). Nothing is committed or pushed here.

import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const src = resolve(here, '../dist-site')
const site = resolve(process.argv[2] ?? '')
if (!process.argv[2] || !existsSync(join(site, 'public')) || !existsSync(join(site, 'build.mjs'))) {
  console.error('usage: node scripts/publish-site.mjs <path to the ogun-site checkout>')
  process.exit(2)
}
if (!existsSync(join(src, 'index.html'))) {
  console.error('build it first: npm run build:site')
  process.exit(2)
}

const dest = join(site, 'public', 'studio')
rmSync(dest, { recursive: true, force: true })
mkdirSync(dest, { recursive: true })
cpSync(src, dest, { recursive: true })

const page = join(dest, 'index.html')
let html = readFileSync(page, 'utf8')
html = html
  .replace('<title>Ogun Studio</title>', '<title>Ogun Studio</title>')
  .replace('<meta name="color-scheme"', '<meta name="robots" content="noindex, nofollow" />\n    <meta name="color-scheme"')
  .replace(/<meta name="description"[^>]*>/, '<meta name="description" content="Ogun Studio: workflows, runs, partitions, approvals, artifacts, lineage and streams." />')
writeFileSync(page, html)

const files = (d) => readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? files(join(d, e.name)) : [join(d, e.name)]))
console.log(`copied ${files(dest).length} files to ${dest}`)
