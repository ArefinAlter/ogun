// Benchmarks on the engine Studio runs for itself (`bench.*`, O-06). The calls go through the same relay as the
// other engine screens (`localSvcTransport`), so the shell's closed method list still decides what is reachable.

import { idempotencyKey } from '../lib/format'
import { benchOffered } from '../lib/bench'
import type { BenchApi } from './backend'
import { asOgunError } from './backend'
import type { SvcTransport } from './svc'

/** The benchmark methods over a transport that speaks the daemon's names. */
export function benchApi(t: SvcTransport): BenchApi {
  return {
    list: () => t.call('bench.list'),
    run: (params, key = idempotencyKey('bench-run')) => t.call('bench.run', { suite: params.suite ?? 'sample', runtime: params.runtime, seeds: params.seeds, idempotency_key: key }),
    status: (id) => t.call('bench.status', { id }),
    cancel: (id, key = idempotencyKey('bench-cancel')) => t.call('bench.cancel', { id, idempotency_key: key }),
    replay: (id, agentRun) => t.call('bench.replay', { id, agent_run: agentRun }),
  }
}

/**
 * Whether this engine offers benchmarks, asked cheaply: `bench.status` for an id that cannot exist fails with
 * `api.invalid_params` where the method is there and `api.method_not_found` where it is not, and runs nothing.
 * `undefined` means the engine could not answer yet.
 */
export async function probeBench(t: SvcTransport): Promise<boolean | undefined> {
  try {
    await t.call('bench.status', { id: 'bmr_probe' })
    return true
  } catch (e) {
    return benchOffered(asOgunError(e))
  }
}
