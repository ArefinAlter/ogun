// The page shown in place of the others while Studio's own engine is not running (ADR 0015). Starting and
// restarting are ordinary, so they get a calm waiting page, not an error; only an engine that could not be kept
// running is shown as a failure, with what happened and where the log is.

import { useState } from 'preact/hooks'

import { asOgunError } from '../api'
import type { EngineInfo, OgunError } from '../api/types'
import { useAsync, useBackend } from '../lib/hooks'
import { ErrorBox, Kv } from '../ui/components'
import { Mark } from '../ui/icons'

/** Whether the page should be held back for this engine. */
export function engineBlocks(engine: EngineInfo | undefined): boolean {
  return !!engine && (engine.state === 'starting' || engine.state === 'restarting' || engine.state === 'failed' || engine.state === 'stopped')
}

/** The sidebar dot: green when running, amber while it starts, red when it could not be kept running. */
export function engineTone(engine: EngineInfo | undefined): 'ok' | 'warn' | 'bad' | '' {
  switch (engine?.state) {
    case 'running':
      return 'ok'
    case 'starting':
    case 'restarting':
      return 'warn'
    case 'failed':
    case 'stopped':
      return 'bad'
    default:
      return ''
  }
}

function Waiting({ engine }: { engine: EngineInfo }) {
  const restarting = engine.state === 'restarting'
  return (
    <div class="engine-screen" role="status" aria-live="polite">
      <div class="hero">
        <Mark />
        <h1>{restarting ? 'Starting the engine again' : 'Starting the engine'}</h1>
        <p class="muted">
          {restarting
            ? `The engine stopped${engine.error ? ` (${engine.error})` : ''}. Studio is starting it again, attempt ${engine.attempt ?? 1}. Nothing you stored is lost: runs pick up where they stopped.`
            : 'Ogun Studio runs its own engine on this computer. The first start creates your workspace and takes a few seconds.'}
        </p>
        <div class="engine-dots" aria-hidden="true">
          <span />
          <span />
          <span />
        </div>
      </div>
    </div>
  )
}

function Failed({ engine }: { engine: EngineInfo }) {
  const backend = useBackend()
  const report = useAsync(() => (backend.engine ? backend.engine.report() : Promise.resolve(null)), [backend, engine.state, engine.restarts])
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  const restart = async () => {
    setBusy(true)
    setError(undefined)
    try {
      await backend.engine?.restart()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const tail = report.data?.log_tail ?? []
  return (
    <div class="engine-screen">
      <div class="hero">
        <Mark />
        <h1>{engine.state === 'stopped' ? 'The engine is stopped' : 'The engine could not be kept running'}</h1>
        <p class="muted">
          {engine.state === 'stopped'
            ? 'Studio is closing the engine. Open Studio again to start it.'
            : 'Studio tried several times and gave up. Your data is untouched: it stays in the state folder below and the engine reads it again at its next start.'}
        </p>
      </div>
      {engine.error ? (
        <div class="notice error" role="alert">
          <div>{engine.error}</div>
        </div>
      ) : null}
      <ErrorBox error={error} title="Restart" />
      <div class="toolbar">
        <button type="button" class="btn primary" disabled={busy || !backend.engine} onClick={() => void restart()}>
          Restart the engine
        </button>
      </div>
      <Kv
        rows={[
          ['state folder', <span class="mono">{engine.state_dir}</span>],
          ['log', <span class="mono">{engine.log_path}</span>],
          ['restarts', String(engine.restarts)],
        ]}
      />
      {tail.length ? (
        <>
          <h2>End of the log</h2>
          <pre class="code-block mono engine-log">{tail.join('\n')}</pre>
        </>
      ) : null}
    </div>
  )
}

export function EngineScreen({ engine }: { engine: EngineInfo }) {
  return engine.state === 'failed' || engine.state === 'stopped' ? <Failed engine={engine} /> : <Waiting engine={engine} />
}
