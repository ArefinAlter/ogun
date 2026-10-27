// What the Benchmarks screen decides from the engine's answers. Pure functions, no browser and no imports besides
// types, so a plain `node --test` can load it (tests/bench.test.mjs).

import type { BenchEvent, BenchReplay, BenchRunState, BenchRunSummary, BenchRuntime, OgunError } from '../api/types'

/** How often a running benchmark is looked at again. */
export const POLL_MS = 2000

/** The most seeds one run may use (the engine refuses more). */
export const MAX_SEEDS = 5

/** How many trajectory events the verification dialog lists. */
export const EVENTS_SHOWN = 14

/** The exact state word, the sentence that says what it means, and a tone for the chip beside it. */
export function stateInfo(state: BenchRunState | string): { word: string; meaning: string; tone: '' | 'ok' | 'warn' | 'bad' | 'accent' } {
  switch (state) {
    case 'running':
      return { word: 'running', meaning: 'The harness is working through the matrix.', tone: 'accent' }
    case 'finished':
      return { word: 'finished', meaning: 'Every run in the matrix has a result.', tone: 'ok' }
    case 'failed':
      return { word: 'failed', meaning: 'The harness stopped before the matrix was done.', tone: 'bad' }
    case 'cancelled':
      return { word: 'cancelled', meaning: 'A person stopped the run; the rows below are what it had finished.', tone: 'warn' }
    case 'interrupted':
      return { word: 'interrupted', meaning: 'The engine stopped while the run was going. Start it again to get a full result.', tone: 'warn' }
    default:
      return { word: String(state), meaning: 'The engine reported a state this Studio does not know.', tone: '' }
  }
}

/** A run that is still going: Studio keeps asking until it is not. */
export function isActive(state: string | undefined): boolean {
  return state === 'running'
}

/** Whether the list (or one run) needs another look in a moment. */
export function shouldPoll(runs: readonly { state: string }[] | undefined, detail?: { state: string } | undefined): boolean {
  if (detail) return isActive(detail.state)
  return !!runs?.some((r) => isActive(r.state))
}

/** "3 of 8 passed", or "no results yet". */
export function passedText(run: Pick<BenchRunSummary, 'runs' | 'passed'>): string {
  return run.runs === 0 ? 'no results yet' : `${run.passed} of ${run.runs} passed`
}

/** Parts per million as a percentage with at most one decimal: 1000000 is "100%", 666666 is "66.6%". */
export function ppmPercent(ppm: number | undefined | null): string {
  if (ppm === undefined || ppm === null || !Number.isFinite(ppm)) return '–'
  const p = Math.floor(ppm / 1000) / 10
  return Number.isInteger(p) ? `${p}%` : `${p.toFixed(1)}%`
}

/** How the network was denied, in words. The key is what a row records as `isolation`. */
export function isolationLabel(isolation: string | null | undefined): string {
  switch (isolation) {
    case 'local_appcontainer':
      return 'Windows AppContainer, no network'
    case 'local_netns':
      return 'network namespace, no network'
    case 'container':
      return 'container, no network'
    case 'unisolated_local':
      return 'not isolated'
    case null:
    case undefined:
    case '':
      return '–'
    default:
      return String(isolation)
  }
}

/** True when the isolation really denied the network (`unisolated_local` did not). */
export function deniesNetwork(isolation: string | null | undefined): boolean {
  return isolation === 'local_appcontainer' || isolation === 'local_netns' || isolation === 'container'
}

/** What the loopback probe count means. A run that is still going has no final number yet. */
export function probeText(run: Pick<BenchRunSummary, 'probe_hits' | 'state'>): { label: string; explain: string; tone: '' | 'ok' | 'bad' } {
  const n = run.probe_hits
  const label = `network probe connections: ${n}`
  if (run.state === 'running') {
    return { label, explain: 'The engine counts the connections its loopback listener accepts while the run goes on; the final number appears when it ends.', tone: '' }
  }
  if (n === 0) {
    return { label, explain: 'Zero means the agent could not reach a loopback listener the engine opened for the run: the network stayed closed.', tone: 'ok' }
  }
  return { label, explain: 'Something inside the run reached the loopback listener the engine opened. The network was not closed for it; do not trust this run as isolated.', tone: 'bad' }
}

/** The runtimes that can be chosen, in the order the engine listed them. */
export function availableRuntimes(runtimes: readonly BenchRuntime[] | undefined): BenchRuntime[] {
  return (runtimes ?? []).filter((r) => r.available)
}

/** The runtime to preselect: the local one when it works, else the first that does; null when none does. */
export function defaultRuntime(runtimes: readonly BenchRuntime[] | undefined): string | null {
  const ok = availableRuntimes(runtimes)
  return (ok.find((r) => r.id === 'local') ?? ok[0])?.id ?? null
}

/** The sentence shown when no runtime can deny the network. Empty when at least one can. */
export function refusalText(runtimes: readonly BenchRuntime[] | undefined, harnessAvailable: boolean): string {
  if (!harnessAvailable) {
    return 'The benchmark harness (ogun-bench) did not answer, so no benchmark can run. It ships next to the engine; reinstall Studio if it is missing.'
  }
  if (availableRuntimes(runtimes).length > 0) return ''
  return 'No runtime on this computer can deny the network to a benchmark run, so Studio refuses to start one: a benchmark runs with the network denied or not at all (ADR 0005). Install Docker or Podman, or use a computer where the local runtime can isolate (Windows with the sandbox helper, Linux with user namespaces).'
}

/** Seeds 1 to n, within what the engine accepts. */
export function seedsFor(count: number): number[] {
  const n = Math.min(MAX_SEEDS, Math.max(1, Math.round(Number.isFinite(count) ? count : 1)))
  return Array.from({ length: n }, (_, i) => i + 1)
}

/** How many rows a matrix of this suite has: tasks times agents times seeds. */
export function expectedRows(tasks: number, agents: number, seeds: number): number {
  return Math.max(0, tasks) * Math.max(0, agents) * Math.max(1, seeds)
}

/** Whether the engine answered `bench.status` with the refusal that proves the method exists (a made-up id), or
 *  that it does not. `undefined` means the engine could not say yet (starting, restarting). */
export function benchOffered(error: Pick<OgunError, 'code' | 'retryable'> | null): boolean | undefined {
  if (!error) return true
  if (error.code === 'api.method_not_found') return false
  if (error.code === 'api.invalid_params' || error.code.startsWith('bench.')) return true
  return undefined
}

/** One line for a trajectory event: its kind and the most telling field. Text in it came from the agent. */
export function describeEvent(e: BenchEvent): string {
  const pick = (...keys: string[]) => {
    for (const k of keys) {
      const v = e[k]
      if (typeof v === 'string' && v) return v
      if (typeof v === 'number' || typeof v === 'boolean') return String(v)
    }
    return ''
  }
  const detail = pick('untrusted_text', 'untrusted_command', 'untrusted_output', 'tool', 'untrusted_args', 'outcome', 'evaluator', 'task_id', 'exit_code', 'files')
  const text = detail.replace(/\s+/g, ' ').slice(0, 140)
  return `#${e.seq} +${e.elapsed_ms} ms  ${e.type}${text ? `  ${text}` : ''}`
}

/** The verification dialog's facts: whether the chain checked out, the event count and the first events. The
 *  headline is a sentence that follows the words "Verified: yes." or "Verified: no.". */
export function replayBrief(r: BenchReplay): { verified: boolean; headline: string; events: string[]; more: number } {
  const total = r.summary?.events ?? r.events.length
  const shown = r.events.slice(0, EVENTS_SHOWN).map(describeEvent)
  return {
    verified: r.verified,
    headline: r.verified
      ? `The hash chain of ${total} event${total === 1 ? '' : 's'} is intact.`
      : `The hash chain did not check out${r.error ? ` (${r.error})` : ''}.`,
    events: shown,
    more: Math.max(0, total - shown.length),
  }
}

/** Newest run first, as the engine sorts them; this keeps a stable order if the list is merged client-side. */
export function newestFirst<T extends { id: string }>(runs: readonly T[]): T[] {
  return [...runs].sort((a, b) => (a.id < b.id ? 1 : a.id > b.id ? -1 : 0))
}
