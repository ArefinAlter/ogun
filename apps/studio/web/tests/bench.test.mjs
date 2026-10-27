import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import ts from 'typescript'

// Compile the real module, with no browser and no extra dependency (it only imports types).
const dir = await mkdtemp(join(tmpdir(), 'ogun-bench-'))
const source = await readFile(new URL('../src/lib/bench.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } })
await writeFile(join(dir, 'bench.mjs'), outputText)
const b = await import(pathToFileURL(join(dir, 'bench.mjs')).href)
after(() => rm(dir, { recursive: true, force: true }))

const runtime = (id, available, isolation = null) => ({ id, title: id, available, isolation, note: '' })

test('every state the engine reports has its exact word, a meaning and a tone', () => {
  for (const state of ['running', 'finished', 'failed', 'cancelled', 'interrupted']) {
    const s = b.stateInfo(state)
    assert.equal(s.word, state)
    assert.ok(s.meaning.length > 10, state)
  }
  assert.equal(b.stateInfo('running').tone, 'accent')
  assert.equal(b.stateInfo('finished').tone, 'ok')
  assert.equal(b.stateInfo('failed').tone, 'bad')
  assert.equal(b.stateInfo('interrupted').tone, 'warn')
  // A state this Studio does not know is shown as sent, not hidden.
  assert.equal(b.stateInfo('paused').word, 'paused')
})

test('only a running run is active', () => {
  assert.equal(b.isActive('running'), true)
  for (const s of ['finished', 'failed', 'cancelled', 'interrupted', undefined]) assert.equal(b.isActive(s), false, String(s))
})

test('polling continues while something runs and stops when nothing does', () => {
  assert.equal(b.POLL_MS, 2000)
  assert.equal(b.shouldPoll(undefined), false)
  assert.equal(b.shouldPoll([]), false)
  assert.equal(b.shouldPoll([{ state: 'finished' }, { state: 'failed' }]), false)
  assert.equal(b.shouldPoll([{ state: 'finished' }, { state: 'running' }]), true)
  // One run on its own page decides by itself, whatever the list says.
  assert.equal(b.shouldPoll([{ state: 'running' }], { state: 'finished' }), false)
  assert.equal(b.shouldPoll(undefined, { state: 'running' }), true)
})

test('the summary of a run says how many passed, or that nothing has a result yet', () => {
  assert.equal(b.passedText({ runs: 8, passed: 4 }), '4 of 8 passed')
  assert.equal(b.passedText({ runs: 1, passed: 0 }), '0 of 1 passed')
  assert.equal(b.passedText({ runs: 0, passed: 0 }), 'no results yet')
})

test('rates are parts per million shown as a percentage, rounded down', () => {
  assert.equal(b.ppmPercent(1_000_000), '100%')
  assert.equal(b.ppmPercent(0), '0%')
  assert.equal(b.ppmPercent(500_000), '50%')
  assert.equal(b.ppmPercent(666_666), '66.6%')
  assert.equal(b.ppmPercent(999_999), '99.9%')
  assert.equal(b.ppmPercent(undefined), '–')
})

test('isolation is named in words and only real isolation counts as denying the network', () => {
  assert.match(b.isolationLabel('local_appcontainer'), /Windows AppContainer/)
  assert.match(b.isolationLabel('local_netns'), /namespace/)
  assert.match(b.isolationLabel('container'), /container/)
  assert.equal(b.isolationLabel('unisolated_local'), 'not isolated')
  assert.equal(b.isolationLabel(null), '–')
  assert.equal(b.deniesNetwork('local_appcontainer'), true)
  assert.equal(b.deniesNetwork('local_netns'), true)
  assert.equal(b.deniesNetwork('container'), true)
  assert.equal(b.deniesNetwork('unisolated_local'), false)
  assert.equal(b.deniesNetwork(null), false)
})

test('the network probe count is worded as the screen promises', () => {
  const closed = b.probeText({ probe_hits: 0, state: 'finished' })
  assert.equal(closed.label, 'network probe connections: 0')
  assert.equal(closed.tone, 'ok')
  assert.match(closed.explain, /Zero means the agent could not reach a loopback listener/)
  const open = b.probeText({ probe_hits: 3, state: 'finished' })
  assert.equal(open.label, 'network probe connections: 3')
  assert.equal(open.tone, 'bad')
  assert.match(open.explain, /not closed/)
  // While the run goes on no verdict is given, even for zero.
  const live = b.probeText({ probe_hits: 0, state: 'running' })
  assert.equal(live.tone, '')
  assert.match(live.explain, /final number appears/)
})

test('the runtime preselected is the local one when it works, else the first that does, else none', () => {
  const rs = [runtime('local', true, 'local_appcontainer'), runtime('docker', true, 'container'), runtime('podman', false)]
  assert.equal(b.defaultRuntime(rs), 'local')
  assert.equal(b.defaultRuntime([runtime('local', false), runtime('docker', true, 'container')]), 'docker')
  assert.equal(b.defaultRuntime([runtime('local', false), runtime('docker', false)]), null)
  assert.equal(b.defaultRuntime(undefined), null)
  assert.deepEqual(b.availableRuntimes(rs).map((r) => r.id), ['local', 'docker'])
})

test('a refusal is explained when no runtime can deny the network, and not otherwise', () => {
  const none = [runtime('local', false), runtime('docker', false), runtime('podman', false)]
  assert.match(b.refusalText(none, true), /refuses to start one/)
  assert.match(b.refusalText(none, true), /network denied or not at all/)
  assert.equal(b.refusalText([runtime('local', true, 'local_netns')], true), '')
  // A harness that did not answer is its own explanation.
  assert.match(b.refusalText([], false), /ogun-bench/)
})

test('seeds stay between one and five', () => {
  assert.deepEqual(b.seedsFor(1), [1])
  assert.deepEqual(b.seedsFor(3), [1, 2, 3])
  assert.deepEqual(b.seedsFor(0), [1])
  assert.deepEqual(b.seedsFor(-4), [1])
  assert.deepEqual(b.seedsFor(99), [1, 2, 3, 4, 5])
  assert.deepEqual(b.seedsFor(2.6), [1, 2, 3])
  assert.deepEqual(b.seedsFor(Number.NaN), [1])
  assert.equal(b.MAX_SEEDS, 5)
})

test('a matrix has tasks times agents times seeds runs', () => {
  assert.equal(b.expectedRows(4, 2, 1), 8)
  assert.equal(b.expectedRows(4, 2, 5), 40)
  assert.equal(b.expectedRows(0, 2, 3), 0)
})

test('the probe for benchmark support reads the engine\'s refusal of an id that cannot exist', () => {
  assert.equal(b.benchOffered(null), true)
  assert.equal(b.benchOffered({ code: 'api.invalid_params', retryable: false }), true)
  assert.equal(b.benchOffered({ code: 'bench.environment_failed', retryable: true }), true)
  assert.equal(b.benchOffered({ code: 'api.method_not_found', retryable: false }), false)
  // An engine that is starting says nothing yet, and Studio asks again later.
  assert.equal(b.benchOffered({ code: 'api.unavailable', retryable: true }), undefined)
})

test('a trajectory event becomes one line and says how much text is the agent\'s', () => {
  const line = b.describeEvent({ seq: 4, at: 'x', elapsed_ms: 182, type: 'agent.output', stream: 'stderr', untrusted_text: 'hello\n  world' })
  assert.equal(line, '#4 +182 ms  agent.output  hello world')
  assert.equal(b.describeEvent({ seq: 1, at: 'x', elapsed_ms: 0, type: 'task.started', task_id: 'count-rows' }), '#1 +0 ms  task.started  count-rows')
  assert.equal(b.describeEvent({ seq: 9, at: 'x', elapsed_ms: 5, type: 'task.finished' }), '#9 +5 ms  task.finished')
  const long = b.describeEvent({ seq: 2, at: 'x', elapsed_ms: 1, type: 'tool.shell', untrusted_command: 'x'.repeat(500) })
  assert.ok(long.length < 200)
})

const events = (n) => Array.from({ length: n }, (_, i) => ({ seq: i + 1, at: 'x', elapsed_ms: i, type: 'tool.call', tool: 'write_file' }))

test('a verified trajectory says so, counts its events and lists only the first few', () => {
  const brief = b.replayBrief({ verified: true, summary: { events: 20, head_blake3: 'a'.repeat(64), tool_calls: 2, failed_tool_calls: 0, agent_ms: 12 }, error: null, events: events(20) })
  assert.equal(brief.verified, true)
  assert.equal(brief.headline, 'The hash chain of 20 events is intact.')
  assert.equal(brief.events.length, b.EVENTS_SHOWN)
  assert.equal(brief.more, 20 - b.EVENTS_SHOWN)
  const one = b.replayBrief({ verified: true, summary: { events: 1, head_blake3: 'b'.repeat(64), tool_calls: 0, failed_tool_calls: 0, agent_ms: 0 }, error: null, events: events(1) })
  assert.equal(one.headline, 'The hash chain of 1 event is intact.')
  assert.equal(one.more, 0)
})

test('a trajectory that does not verify says why and is never presented as intact', () => {
  const brief = b.replayBrief({ verified: false, summary: null, error: 'bench.trajectory_broken at line 3', events: [] })
  assert.equal(brief.verified, false)
  assert.match(brief.headline, /did not check out/)
  assert.match(brief.headline, /line 3/)
  assert.doesNotMatch(brief.headline, /intact/)
  assert.equal(b.replayBrief({ verified: false, summary: null, error: null, events: [] }).headline, 'The hash chain did not check out.')
})

test('runs sort newest first by their id', () => {
  const sorted = b.newestFirst([{ id: 'bmr_01A' }, { id: 'bmr_01C' }, { id: 'bmr_01B' }])
  assert.deepEqual(sorted.map((r) => r.id), ['bmr_01C', 'bmr_01B', 'bmr_01A'])
})
