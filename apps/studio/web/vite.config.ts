import { defineConfig } from 'vite'

// One build serves every shell: the Tauri desktop and mobile webviews load it from the bundled assets, and
// ogun-server serves the same files as Studio-web. Relative asset URLs keep it independent of the mount point.
// No plugin is needed: esbuild compiles TSX for Preact's automatic JSX runtime.
export default defineConfig({
  base: './',
  define: { __APP_VERSION__: JSON.stringify(process.env.npm_package_version ?? '0.1.0') },
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
