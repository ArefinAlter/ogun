// Picks the backend: inside a Tauri shell the closed command set, otherwise Studio-web's REST routes. The
// scripted demo (`src/demo`) is chosen only by an explicit switch, and loads as its own chunk so the shells
// never carry it.

import type { Backend } from './backend'
import { asOgunError } from './backend'
import { benchApi, probeBench } from './bench'
import { currentCsrf, httpBackend } from './http'
import { realSchedules } from './schedules'
import { fetchTransport, svcInfo, svcParts } from './svc'
import { localEngineApi, localHubsApi, localSvcTransport, tauriBackend, tauriSvcTransport } from './tauri'

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

/** The desktop shell. By default it is linked to the engine it runs for itself (ADR 0015): the daemon's own
 * methods, through the shell's closed relay, give the agent, connections, run defaults, providers, storage and the
 * rest, and the shell adds the engine's state, the bundled samples and secret entry. Linked to a hub instead, it
 * also reaches that hub's pre-alpha service, when the hub has one. A change of link reloads the page so the right
 * parts are built. */
async function desktop(): Promise<Backend> {
  const b = tauriBackend()
  const info = await b.info().catch(() => null)
  const own = info && !info.mobile ? { engine: localEngineApi(), hubs: localHubsApi() } : {}
  const out: Backend = { ...b, ...realSchedules(b), ...own }
  if (info?.link === 'local' && !info.mobile) {
    // Built whether or not the engine has answered yet: the page is held back while it starts (views/Engine.tsx),
    // and a call before it is ready fails with a retryable error rather than a missing screen.
    const t = localSvcTransport()
    Object.assign(out, svcParts(t, { preview: true }))
    out.setSchedulerEnabled = async (enabled) => void (await t.call('scheduler.set_enabled', { enabled }))
    out.setWorkflowEnabled = async (name, enabled) => void (await t.call('workflows.set_enabled', { name, enabled }))
    // Whether this engine runs agent threads is asked once, when it is ready; the sidebar and the Agent page follow
    // the answer (a daemon without them says so, and Studio shows the page as unavailable instead of a dead chat).
    let probe: Promise<boolean> | null = null
    const agentOffered = () =>
      (probe ??= t.call('agent.tools').then(
        () => true,
        (e) => {
          if (asOgunError(e).code === 'api.method_not_found') return false
          probe = null
          throw e
        },
      ))
    // Benchmarks: the same question, asked with an id that cannot exist (nothing runs). The answer is kept once
    // it is known, so a restart of the engine does not make the page flicker out of the sidebar.
    out.bench = benchApi(t)
    let benchKnown: boolean | undefined
    const baseInfo = out.info
    out.info = async () => {
      const i = await baseInfo()
      if (i.link === 'local' && i.engine?.state === 'running') {
        const ok = await agentOffered().catch(() => undefined)
        if (ok !== undefined) i.agent = ok
        benchKnown ??= await probeBench(t)
      }
      if (i.link === 'local' && benchKnown !== undefined) i.bench = benchKnown
      return i
    }
  }
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
