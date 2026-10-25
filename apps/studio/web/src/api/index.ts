// Picks the backend: inside a Tauri shell the closed command set, otherwise Studio-web's REST routes. The
// scripted demo (`src/demo`) is chosen only by an explicit switch, and loads as its own chunk so the shells
// never carry it.

import type { Backend } from './backend'
import { httpBackend } from './http'
import { tauriBackend } from './tauri'

export function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

/** `VITE_OGUN_DEMO=1` (the `demo` mode) or `?demo` in the page URL. Inside a Tauri shell only the build made for the
 *  demo desktop app (`VITE_OGUN_DESKTOP_DEMO=1`), so a real Studio can never be switched to scripted data. */
export function demoRequested(): boolean {
  if (inTauri()) return import.meta.env.VITE_OGUN_DESKTOP_DEMO === '1'
  if (import.meta.env.VITE_OGUN_DEMO === '1') return true
  return typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('demo')
}

export async function pickBackend(): Promise<Backend> {
  if (demoRequested()) {
    const { demoBackend } = await import('../demo')
    return demoBackend()
  }
  return inTauri() ? tauriBackend() : httpBackend()
}

export type { Backend, DemoControls, PreparedDecision, ShellInfo } from './backend'
export { asOgunError } from './backend'
