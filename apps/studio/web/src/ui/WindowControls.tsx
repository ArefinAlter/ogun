// Minimize, maximize or restore, and close, drawn by the page in the top bar (see lib/shell.ts). Thin glyphs,
// 46 px wide hit areas the height of the bar, and the system's red under the pointer on close, as on Windows.

import { useCallback, useEffect, useState } from 'preact/hooks'

import { currentWindow } from '../lib/shell'

function Glyph({ name }: { name: 'min' | 'max' | 'restore' | 'close' }) {
  return (
    <svg viewBox="0 0 10 10" width="10" height="10" fill="none" stroke="currentColor" stroke-width="1" aria-hidden="true">
      {name === 'min' ? <path d="M0 5.5h10" /> : null}
      {name === 'max' ? <rect x="0.5" y="0.5" width="9" height="9" rx="1" /> : null}
      {name === 'restore' ? (
        <>
          <rect x="0.5" y="2.5" width="7" height="7" rx="1" />
          <path d="M2.5 2.5v-1a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1" />
        </>
      ) : null}
      {name === 'close' ? <path d="M0.5 0.5l9 9M9.5 0.5l-9 9" /> : null}
    </svg>
  )
}

export function WindowControls() {
  const [maximized, setMaximized] = useState(false)
  const sync = useCallback(() => {
    void currentWindow().then((w) => w?.isMaximized().then(setMaximized, () => undefined))
  }, [])
  useEffect(() => {
    sync()
    window.addEventListener('resize', sync)
    return () => window.removeEventListener('resize', sync)
  }, [sync])
  const act = (f: (w: NonNullable<Awaited<ReturnType<typeof currentWindow>>>) => Promise<void>) => () => {
    void currentWindow().then((w) => (w ? f(w) : undefined))
  }
  return (
    <div class="winctl" role="group" aria-label="Window controls">
      <button type="button" class="min" aria-label="Minimize" title="Minimize" onClick={act((w) => w.minimize())}>
        <Glyph name="min" />
      </button>
      <button type="button" class="max" aria-label={maximized ? 'Restore' : 'Maximize'} title={maximized ? 'Restore' : 'Maximize'} onClick={act((w) => w.toggleMaximize())}>
        <Glyph name={maximized ? 'restore' : 'max'} />
      </button>
      <button type="button" class="close" aria-label="Close" title="Close" onClick={act((w) => w.close())}>
        <Glyph name="close" />
      </button>
    </div>
  )
}
