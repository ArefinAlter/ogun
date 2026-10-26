// Benchmarks (O-06, ADR 0005): run the engine's sample suite with the network denied, and read what it produced.
// What is real is said first and plainly: two scripted agents, not models; a check of the harness, not of any model.

import { useCallback, useEffect, useState } from 'preact/hooks'

import { asOgunError } from '../api'
import type { BenchReplay, BenchRow, BenchRuntimeId, OgunError } from '../api/types'
import {
  EVENTS_SHOWN,
  MAX_SEEDS,
  POLL_MS,
  availableRuntimes,
  defaultRuntime,
  deniesNetwork,
  expectedRows,
  isActive,
  isolationLabel,
  passedText,
  ppmPercent,
  probeText,
  refusalText,
  replayBrief,
  seedsFor,
  shouldPoll,
  stateInfo,
} from '../lib/bench'
import { fmtAgo, fmtDuration, fmtTime } from '../lib/format'
import { href, navigate, useAsync, useBackend, useInfo } from '../lib/hooks'
import { Chip, CopyButton, Dialog, Empty, ErrorBox, Hash, Kv, Loading, Notice, PageHead, Segmented, SettingRow, SettingsGroup, Untrusted } from '../ui/components'
import { Icon } from '../ui/icons'

/** Asks again every couple of seconds while `active`. */
function usePolling(active: boolean, again: () => void) {
  useEffect(() => {
    if (!active) return undefined
    const t = setInterval(again, POLL_MS)
    return () => clearInterval(t)
  }, [active, again])
}

function RunState({ state }: { state: string }) {
  const s = stateInfo(state)
  return (
    <span class={`state s-${s.word}`} title={s.meaning}>
      {s.word}
    </span>
  )
}

function Unavailable() {
  return (
    <>
      <PageHead title="Benchmarks" />
      <Notice>
        Benchmarks run on the engine Studio starts for itself, with the network denied to every run. This environment has no such engine (a hub, the browser build and the scripted build do not run
        benchmarks), or the engine has no benchmark methods.
      </Notice>
    </>
  )
}

/** What is and is not real, before anything else. */
function WhatIsReal({ adapters }: { adapters: string[] }) {
  return (
    <SettingsGroup title="What this is">
      <div class="row stack">
        <div class="text">
          <div class="title">A check of the harness, not of any model</div>
          <ul class="bench-facts">
            <li>
              The sample suite has four small tasks and <strong>two scripted agents, not models</strong>: a reference script that solves them and an idle script that does nothing. No model is called and no
              key is used.
            </li>
            <li>
              Every run gets a fresh workspace and the <strong>network denied</strong>. One task passes only if a loopback listener the engine opened was not reachable from inside the run; the count of
              connections it saw is shown beside each run.
            </li>
            <li>An evaluator scores each result. Each run leaves a hash-chained trajectory you can verify here, and lineage events, in a folder outside Ogun's own state: a benchmark run never reads your runs, artifacts, secrets or Zeus.</li>
            <li>It says nothing about how well any model performs.</li>
            <li>
              The engine also has adapters for {adapters.length ? adapters.join(', ') : 'Harbor, Spider 2.0-DBT and ADE-bench'}, but none is offered here: they need the benchmarks' own task checkouts and a
              container runtime, and their layouts are not yet verified against the upstream projects.
            </li>
          </ul>
        </div>
      </div>
    </SettingsGroup>
  )
}

export function Benchmarks() {
  const backend = useBackend()
  const { info } = useInfo()
  const bench = backend.bench
  const list = useAsync(() => (bench ? bench.list() : Promise.resolve(null)), [backend])
  const [runtime, setRuntime] = useState<BenchRuntimeId | null>(null)
  const [seeds, setSeeds] = useState(1)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  const data = list.data
  const reload = list.reload
  const again = useCallback(() => reload(), [reload])
  usePolling(shouldPoll(data?.runs), again)
  useEffect(() => {
    if (runtime === null && data) setRuntime(defaultRuntime(data.runtimes) as BenchRuntimeId | null)
  }, [data, runtime])
  if (!bench || info?.bench === false) return <Unavailable />

  const suite = data?.suites[0]
  const available = availableRuntimes(data?.runtimes)
  const refusal = data ? refusalText(data.runtimes, data.available) : ''
  const rows = suite ? expectedRows(suite.tasks.length, suite.agents.length, seeds) : 0
  const start = async () => {
    if (!runtime) return
    setBusy(true)
    setError(undefined)
    try {
      const r = await bench.run({ runtime, seeds: seedsFor(seeds), ...(suite ? { suite: suite.id } : {}) })
      navigate(href('benchmarks', r.id))
    } catch (e) {
      setError(asOgunError(e))
      reload()
    } finally {
      setBusy(false)
    }
  }
  return (
    <>
      <PageHead title="Benchmarks" sub="Run a matrix of agents and tasks in an isolated environment with the network denied, then read the results.">
        <button type="button" class="btn" onClick={() => reload()} disabled={list.loading}>
          <Icon name="refresh" /> Refresh
        </button>
      </PageHead>
      <ErrorBox error={list.error} title="Benchmarks" />
      <ErrorBox error={error} title="Run" />
      {list.loading && !data ? <Loading /> : null}
      {data ? (
        <>
          <WhatIsReal adapters={data.adapters} />

          <SettingsGroup title="Where a run can execute" note="A run is refused rather than started with less isolation than it asks for: the network is denied by default and Studio never relaxes that.">
            {data.runtimes.map((r) => (
              <SettingRow
                key={r.id}
                title={
                  <>
                    {r.title} {r.available ? <Chip tone="ok">available</Chip> : <Chip tone="warn">not available</Chip>}
                    {r.available ? <Chip>{isolationLabel(r.isolation)}</Chip> : null}
                  </>
                }
                desc={r.note}
              />
            ))}
            {refusal ? (
              <div class="row stack">
                <div class="text">
                  <Notice kind="warn">{refusal}</Notice>
                </div>
              </div>
            ) : null}
          </SettingsGroup>

          <SettingsGroup title="Run">
            <SettingRow
              title={suite?.title ?? 'Sample suite'}
              desc={
                suite
                  ? `${suite.tasks.length} tasks (${suite.tasks.join(', ')}) and ${suite.agents.length} agents (${suite.agents.join(', ')}): ${rows} runs with ${seeds} seed${seeds === 1 ? '' : 's'}.`
                  : 'The engine offers no suite.'
              }
            >
              <span class="inline">
                {available.length > 1 ? (
                  <Segmented<BenchRuntimeId> label="Runtime" value={(runtime ?? available[0]?.id ?? 'local') as BenchRuntimeId} options={available.map((r) => [r.id, r.title] as [BenchRuntimeId, string])} onChange={setRuntime} />
                ) : null}
                <label class="withunit">
                  <span class="muted">Seeds</span>
                  <input
                    type="number"
                    aria-label="Seeds, 1 to 5"
                    min={1}
                    max={MAX_SEEDS}
                    value={seeds}
                    onChange={(e) => setSeeds(seedsFor(Number((e.target as HTMLInputElement).value)).length)}
                  />
                </label>
                <button type="button" class="btn primary" disabled={busy || !runtime || available.length === 0 || !suite} onClick={() => void start()}>
                  <Icon name="play" /> {busy ? 'Starting…' : 'Run'}
                </button>
              </span>
            </SettingRow>
          </SettingsGroup>

          <section class="sgroup">
            <h2 class="sgroup-title">Runs</h2>
            {data.runs.length === 0 ? (
              <Empty>No benchmark has run yet.</Empty>
            ) : (
              <div class="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Run</th>
                      <th>State</th>
                      <th>Result</th>
                      <th class="hide-narrow">Isolation</th>
                      <th class="hide-narrow">Network probe</th>
                      <th>Started</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.runs.map((r) => (
                      <tr key={r.id}>
                        <td>
                          <a class="mono" href={href('benchmarks', r.id)}>
                            {r.id}
                          </a>
                        </td>
                        <td>
                          <RunState state={r.state} />
                        </td>
                        <td>{passedText(r)}</td>
                        <td class="hide-narrow">{isolationLabel(r.isolation)}</td>
                        <td class="hide-narrow" title={probeText(r).explain}>
                          {r.probe_hits}
                        </td>
                        <td title={fmtTime(r.started_at)}>{fmtAgo(r.started_at)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>
        </>
      ) : null}
    </>
  )
}

/** The dialog behind "Verify trajectory": the engine checks the hash chain again and returns the first events. */
function VerifyDialog({ runId, row, onClose }: { runId: string; row: BenchRow; onClose: () => void }) {
  const backend = useBackend()
  const check = useAsync(() => (backend.bench ? backend.bench.replay(runId, row.agent_run) : Promise.reject(new Error('no benchmarks'))), [backend, runId, row.agent_run])
  const unsupported = check.error?.code === 'api.method_not_found'
  const brief = check.data ? replayBrief(check.data as BenchReplay) : null
  return (
    <Dialog title="Verify trajectory" onClose={onClose}>
      <p class="muted">
        {row.task_id} · {row.agent} · seed {row.seed} · <span class="mono">{row.agent_run}</span>
      </p>
      {check.loading && !check.data ? <Loading what="checking the hash chain" /> : null}
      {unsupported ? <Notice>This engine cannot re-check a trajectory from Studio. <span class="mono">ogun-bench replay</span> does it on a terminal.</Notice> : <ErrorBox error={check.error} />}
      {brief ? (
        <>
          <Notice kind={brief.verified ? 'info' : 'error'}>
            <strong>{brief.verified ? 'Verified: yes.' : 'Verified: no.'}</strong> {brief.headline}
          </Notice>
          {check.data?.summary ? (
            <Kv
              rows={[
                ['Events', String(check.data.summary.events)],
                ['Chain head', <Hash value={check.data.summary.head_blake3} short />],
                ['Tool calls', `${check.data.summary.tool_calls} (${check.data.summary.failed_tool_calls} failed)`],
                ['Agent time', fmtDuration(check.data.summary.agent_ms)],
              ]}
            />
          ) : null}
          <Untrusted label={`First ${Math.min(EVENTS_SHOWN, brief.events.length)} events`} text={brief.events.join('\n')} />
          {brief.more > 0 ? <p class="muted">and {brief.more} more.</p> : null}
        </>
      ) : null}
      <div class="toolbar">
        <button type="button" class="btn" onClick={onClose}>
          Close
        </button>
      </div>
    </Dialog>
  )
}

export function BenchmarkDetail({ runId }: { runId: string }) {
  const backend = useBackend()
  const { info } = useInfo()
  const bench = backend.bench
  const status = useAsync(() => (bench ? bench.status(runId) : Promise.resolve(null)), [backend, runId])
  const [error, setError] = useState<OgunError | undefined>()
  const [cancelling, setCancelling] = useState(false)
  const [verify, setVerify] = useState<BenchRow | null>(null)
  const reload = status.reload
  const again = useCallback(() => reload(), [reload])
  const s = status.data
  usePolling(shouldPoll(undefined, s?.run), again)
  if (!bench || info?.bench === false) return <Unavailable />

  const run = s?.run
  const cancel = async () => {
    setCancelling(true)
    setError(undefined)
    try {
      await bench.cancel(runId)
      reload()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setCancelling(false)
    }
  }
  const info2 = run ? stateInfo(run.state) : null
  const probe = run ? probeText(run) : null
  return (
    <>
      <PageHead title="Benchmark run" sub={run ? <span class="mono">{run.id}</span> : undefined}>
        <a class="btn" href={href('benchmarks')}>
          All runs
        </a>
        {run && isActive(run.state) ? (
          <button type="button" class="btn danger" disabled={cancelling} onClick={() => void cancel()}>
            {cancelling ? 'Cancelling…' : 'Cancel the run'}
          </button>
        ) : null}
      </PageHead>
      <ErrorBox error={status.error} />
      <ErrorBox error={error} title="Cancel" />
      {status.loading && !s ? <Loading /> : null}
      {s && run && info2 && probe ? (
        <>
          <Kv
            rows={[
              ['State', <><RunState state={run.state} /> <span class="muted">{info2.meaning}</span></>],
              ['Result', `${passedText(run)}${run.state === 'running' ? ' so far' : ''}`],
              ['Runtime', run.runtime === 'local' ? 'This computer, network denied' : `${run.runtime} container, network denied`],
              ['Isolation', <>{isolationLabel(run.isolation)} {run.isolation && !deniesNetwork(run.isolation) ? <Chip tone="bad">network not denied</Chip> : null}</>],
              ['Network probe', <><span class={`probe ${probe.tone}`}>{probe.label}</span><div class="muted">{probe.explain}</div></>],
              ['Seeds', s.run.seeds.join(', ')],
              ['Started', fmtTime(run.started_at)],
              ['Finished', run.finished_at ? fmtTime(run.finished_at) : '–'],
              ['Matrix', <Hash value={s.matrix_blake3 ?? undefined} short />],
            ]}
          />
          {run.error ? <Notice kind="error">{run.error}</Notice> : null}
          {run.state === 'failed' && s.log_tail ? (
            <details class="panel bench-log">
              <summary>The harness's last log lines</summary>
              <pre class="code-block mono">{s.log_tail}</pre>
            </details>
          ) : null}

          <section class="sgroup">
            <h2 class="sgroup-title">Agents</h2>
            {s.agents.length === 0 ? (
              <Empty>{run.state === 'running' ? 'The first result is on its way.' : 'No agent finished a run.'}</Empty>
            ) : (
              <div class="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Agent</th>
                      <th class="num">Runs</th>
                      <th class="num">Passed</th>
                      <th class="num">Pass rate</th>
                      <th class="num">Mean agent time</th>
                      <th class="num hide-narrow">Errors</th>
                    </tr>
                  </thead>
                  <tbody>
                    {s.agents.map((a) => (
                      <tr key={`${a.agent}@${a.agent_version}`}>
                        <td>
                          {a.agent} <span class="muted">v{a.agent_version}</span> <Chip>script, not a model</Chip>
                        </td>
                        <td class="num">{a.runs}</td>
                        <td class="num">{a.passed}</td>
                        <td class="num">{ppmPercent(a.pass_rate_ppm)}</td>
                        <td class="num">{fmtDuration(a.mean_agent_ms)}</td>
                        <td class="num hide-narrow">{a.errors}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          <section class="sgroup">
            <h2 class="sgroup-title">Runs in the matrix</h2>
            {s.rows.length === 0 ? (
              <Empty>No run has a result yet.</Empty>
            ) : (
              <div class="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Task</th>
                      <th>Agent</th>
                      <th class="num hide-narrow">Seed</th>
                      <th>Outcome</th>
                      <th>Passed</th>
                      <th class="hide-narrow">Isolation</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {s.rows.map((r) => (
                      <tr key={r.agent_run}>
                        <td>{r.task_id}</td>
                        <td>{r.agent}</td>
                        <td class="num hide-narrow">{r.seed}</td>
                        <td>
                          <span class={`state s-${r.outcome}`}>{r.outcome}</span>
                          {r.error_code ? <span class="muted"> {r.error_code}</span> : null}
                        </td>
                        <td>
                          <span class={`state s-${r.passed ? 'passed' : 'notpassed'}`}>{r.passed ? 'passed' : 'not passed'}</span>
                        </td>
                        <td class="hide-narrow">{isolationLabel(r.isolation)}</td>
                        <td class="num">
                          <button type="button" class="btn small" onClick={() => setVerify(r)}>
                            Verify trajectory
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          {s.markdown ? (
            <details class="panel bench-log">
              <summary>Results table as Markdown</summary>
              <div class="toolbar">
                <CopyButton text={s.markdown} label="Copy the table" />
              </div>
              <pre class="code-block mono">{s.markdown}</pre>
            </details>
          ) : null}
        </>
      ) : null}
      {verify ? <VerifyDialog runId={runId} row={verify} onClose={() => setVerify(null)} /> : null}
    </>
  )
}
