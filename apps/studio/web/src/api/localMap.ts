// The engine Studio runs for itself speaks the daemon's own method names (O-16, O-17), while the screens were
// written against the hosted service's names (`svc.ts`). This file is the whole translation, as pure data and
// one function, so a test can pin it. Nothing here touches the webview or the network.

export interface DaemonCall {
  method: string
  params: Record<string, unknown>
}

/** Where the service's name and the daemon's name differ. */
export const RENAMED: Readonly<Record<string, string>> = {
  'agent.settings': 'agent.settings.get',
  'agent.set_settings': 'agent.settings.put',
  'agent.threads': 'agent.threads.list',
  'agent.thread': 'agent.threads.get',
  'agent.create': 'agent.threads.create',
  'agent.set_access': 'agent.access.set',
  'agent.rename': 'agent.threads.rename',
  'agent.remove': 'agent.threads.delete',
  'connections.remove': 'connections.delete',
  environments: 'environments.list',
}

/** Daemon methods that change something and take an `idempotency_key` (O-15 section 5). `agent.act` carries its own. */
export const KEYED: ReadonlySet<string> = new Set([
  'agent.settings.put',
  'connections.put',
  'connections.delete',
  'runtime.set',
  'runtime.reset',
  'ladder.put',
  'scheduler.set_enabled',
  'schedules.add',
])

/** How long a thread watcher may wait for a change before the daemon answers with the current state. */
export const WATCH_WAIT_MS = 25_000

/**
 * Translates one call of the service's vocabulary into the daemon's: renames the method, reshapes the two calls
 * whose parameters differ (`connections.put`), adds the long-poll wait to the thread watchers, and adds a fresh
 * idempotency key to every keyed mutation that did not bring one.
 *
 * @param newKey makes a key for the named action, for example `idempotencyKey` from `lib/format`
 */
export function toDaemonCall(method: string, params: Record<string, unknown> | undefined, newKey: (action: string) => string): DaemonCall {
  const target = RENAMED[method] ?? method
  let p: Record<string, unknown> = { ...(params ?? {}) }
  if (method === 'connections.put' && 'input' in p) {
    const { input, ...rest } = p
    p = { ...rest, connection: input }
  }
  if (target === 'agent.threads.list' || target === 'agent.threads.get') p = { wait_ms: WATCH_WAIT_MS, ...p }
  if (KEYED.has(target) && typeof p.idempotency_key !== 'string') p.idempotency_key = newKey(target.replace(/\./g, '-'))
  return { method: target, params: p }
}
