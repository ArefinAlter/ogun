import { defineConfig, type Plugin } from 'vite'

// The Tauri shell freezes Object.prototype (`freezePrototype` in tauri.conf.json), which stops a script from
// planting members that every object would inherit. A frozen prototype makes `{}.constructor = x` throw in strict
// mode, and Preact does exactly that when a component returns a top-level fragment: it copies the vnode, whose
// own `constructor: undefined` member is an ordinary key, with `for (k in src) dst[k] = src[k]`. The webview then
// stays blank. This keeps the freeze and changes only that copy: a key the prototype already has is defined on
// the copy instead of assigned. If a Preact upgrade changes the copy, the build fails rather than ship a blank
// window; tools/ui-reference/smoke_studio.py --freeze-prototype is the end-to-end check.
const ASSIGN = 'function m(n,l){for(var u in l)n[u]=l[u];return n}'
const ASSIGN_SAFE =
  'function m(n,l){for(var u in l)u in Object.prototype?Object.defineProperty(n,u,{value:l[u],writable:!0,enumerable:!0,configurable:!0}):n[u]=l[u];return n}'

function preactFrozenPrototype(): Plugin {
  return {
    name: 'ogun:preact-frozen-prototype',
    enforce: 'pre',
    transform(code, id) {
      if (!/[\\/]preact[\\/]dist[\\/]preact\.module\.js$/.test(id.split('?')[0])) return null
      if (!code.includes(ASSIGN)) {
        throw new Error(
          'preact.module.js no longer has the object copy this build patches (vite.config.ts); check it against a frozen Object.prototype and update the plugin',
        )
      }
      return { code: code.replace(ASSIGN, ASSIGN_SAFE), map: null }
    },
  }
}

// One build serves every shell: the Tauri desktop and mobile webviews load it from the bundled assets, and
// ogun-server serves the same files as Studio-web. Relative asset URLs keep it independent of the mount point.
// esbuild compiles TSX for Preact's automatic JSX runtime; the one plugin is the frozen-prototype fix above.
export default defineConfig({
  base: './',
  define: { __APP_VERSION__: JSON.stringify(process.env.npm_package_version ?? '0.1.0') },
  plugins: [preactFrozenPrototype()],
  // Served unbundled in dev so the plugin above sees Preact there too (`tauri dev` freezes the prototype as well).
  optimizeDeps: { exclude: ['preact', 'preact/hooks', 'preact/jsx-runtime'] },
  esbuild: { jsx: 'automatic', jsxImportSource: 'preact' },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false,
    // Fonts and images ship as files, never as data: URIs, so a strict CSP needs no exceptions.
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false },
  },
  server: {
    port: 5173,
    strictPort: true,
    // For Studio-web development against a running ogun-server: OGUN_SERVER=https://hub.example:8443 npm run dev
    proxy: process.env.OGUN_SERVER
      ? { '/api': { target: process.env.OGUN_SERVER, changeOrigin: true } }
      : undefined,
  },
  clearScreen: false,
})
