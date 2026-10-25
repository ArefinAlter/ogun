// Bundles the service into one ES module for Node 22: `node build.mjs` writes dist/svc.mjs.
// It reuses Studio's own code (the scripted agent, the settings state, the connector catalogue), so esbuild comes
// from the Studio web package: run `npm ci` in apps/studio/web first.
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const here = fileURLToPath(new URL('.', import.meta.url))
const require = createRequire(new URL('../../../apps/studio/web/package.json', import.meta.url))
const esbuild = require('esbuild')

await esbuild.build({
  entryPoints: [here + 'src/main.ts'],
  outfile: here + 'dist/svc.mjs',
  bundle: true,
  platform: 'node',
  format: 'esm',
  target: 'node22',
  // The web modules read Vite's env: no storage, no scripted daemon switches.
  define: {
    'import.meta.env': JSON.stringify({ VITE_OGUN_NO_STORAGE: '1', MODE: 'service' }),
    __APP_VERSION__: JSON.stringify(process.env.OGUN_PREALPHA_VERSION ?? '0.0.1-alpha'),
  },
  external: ['node:*'],
  banner: { js: "import { createRequire as __cr } from 'node:module'; const require = __cr(import.meta.url);" },
  legalComments: 'none',
  logLevel: 'info',
})
