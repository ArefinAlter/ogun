// What kind of window Studio runs in, for the one thing the page has to do about it: draw its own window controls.
//
// Inside the Windows shell the main window has no native title bar (`decorations: false` in tauri.windows.conf.json),
// so the top bar of the page is the title bar and the page draws minimize, maximize and close, as T3 Code does. On
// macOS the shell asks for an overlay title bar: the system draws the traffic lights over the page, which only has to
// leave room for them. Linux keeps its native frame. In a browser and on phones there is no window to control.

import { useEffect, useState } from 'preact/hooks'

export type WindowChrome = 'custom' | 'overlay' | null

/** True inside a Tauri desktop shell (not a browser, not the mobile shells). */
export function desktopOs(): 'windows' | 'macos' | 'linux' | null {
  if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return null
  const ua = navigator.userAgent
  if (/Android|iPhone|iPad/.test(ua)) return null
  if (/Windows/.test(ua)) return 'windows'
  if (/Mac/.test(ua)) return 'macos'
  if (/Linux|X11/.test(ua)) return 'linux'
  return null
}

interface TauriWindow {
  minimize(): Promise<void>
  toggleMaximize(): Promise<void>
  close(): Promise<void>
  isMaximized(): Promise<boolean>
  isDecorated(): Promise<boolean>
}

let cached: Promise<TauriWindow | null> | undefined

/** The current window, loaded only inside a Tauri shell so that the web build never carries the window API. */
export function currentWindow(): Promise<TauriWindow | null> {
  if (!desktopOs()) return Promise.resolve(null)
  cached ??= import('@tauri-apps/api/window').then(
    (m) => m.getCurrentWindow() as unknown as TauriWindow,
    () => null,
  )
  return cached
}

/** `custom` when this window has no native frame and the page must draw the controls; `overlay` on macOS. */
export function useWindowChrome(): WindowChrome {
  const os = desktopOs()
  const [chrome, setChrome] = useState<WindowChrome>(os === 'macos' ? 'overlay' : null)
  useEffect(() => {
    if (os !== 'windows') return
    let alive = true
    void currentWindow().then(async (w) => {
      if (!w || !alive) return
      // A window that still has its native frame (an older config) must not get a second set of buttons.
      const decorated = await w.isDecorated().catch(() => true)
      if (alive && !decorated) setChrome('custom')
    })
    return () => {
      alive = false
    }
  }, [os])
  return chrome
}
