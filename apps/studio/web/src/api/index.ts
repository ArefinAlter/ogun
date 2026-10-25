// Picks the backend: inside a Tauri shell the closed command set, otherwise Studio-web's REST routes. The
// scripted demo (`src/demo`) is chosen only by an explicit switch, and loads as its own chunk so the shells
// never carry it.

import type { Backend } from './backend'
import { currentCsrf, httpBackend } from './http'
import { realSchedules } from './schedules'
import { fetchTransport, svcInfo, svcParts } from './svc'
import { tauriBackend, tauriSvcTransport } from './tauri'

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
  return inTauri() ? desktop() : web(httpBackend())
}

/** Studio-web: the server's routes, schedules read from the daemon, and the pre-alpha service where it runs. */
async function web(b: Backend): Promise<Backend> {
  const out: Backend = { ...b, ...realSchedules(b) }
  if (await svcInfo()) Object.assign(out, svcParts(fetchTransport(currentCsrf)))
  const select = b.selectWorkspace
  if (select) out.selectWorkspace = (id) => {
    const next = select(id)
    // A workspace switch keeps the extras: build them again on the new backend.
    const wrapped: Backend = { ...next, ...realSchedules(next) }
    void svcInfo().then((i) => i && Object.assign(wrapped, svcParts(fetchTransport(currentCsrf))))
    return wrapped
  }
  return out
}

/** A random id for this desktop install, so installs that share the built-in hub token keep their own threads. */
function installId(): string {
  const KEY = 'ogun.studio.install'
  try {
    const have = window.localStorage.getItem(KEY)
    if (have && /^[A-Za-z0-9_-]{16,64}$/.test(have)) return have
    const bytes = crypto.getRandomValues(new Uint8Array(16))
    const id = Array.from(bytes, (x) => x.toString(16).padStart(2, '0')).join('')
    window.localStorage.setItem(KEY, id)
    return id
  } catch {
    return 'default'
  }
}

/** The desktop shell. Linked to a hub, it also reaches that hub's pre-alpha service, when the hub has one. A change
 * of link reloads the page so the right parts are built. */
async function desktop(): Promise<Backend> {
  const b = tauriBackend()
  const out: Backend = { ...b, ...realSchedules(b) }
  const info = await b.info().catch(() => null)
  if (info?.link === 'hub' && !info.mobile) {
    const t = tauriSvcTransport(installId())
    const ok = await t.call('agent.tools').then(() => true, () => false)
    if (ok) Object.assign(out, svcParts(t))
  }
  const reload = <A extends unknown[], R>(f?: (...a: A) => Promise<R>) =>
    f
      ? async (...a: A) => {
          const r = await f(...a)
          setTimeout(() => window.location.reload(), 50)
          return r
        }
      : undefined
  const linkLocal = reload(b.linkLocal)
  const linkHub = reload(b.linkHub)
  const linkForget = reload(b.linkForget)
  if (linkLocal) out.linkLocal = linkLocal
  if (linkHub) out.linkHub = linkHub
  if (linkForget) out.linkForget = linkForget
  return out
}

export type { Backend, DemoControls, PreparedDecision, ShellInfo } from './backend'
export { asOgunError } from './backend'
