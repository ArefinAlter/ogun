// First run on the desktop (ADR 0015): a new workspace is empty, so Studio says what it is and offers the bundled
// sample projects. Importing is `workflows.put` for each file, exactly as `ogun put` does; the daemon validates,
// hashes and stores them, and a version that changes code waits for a person's approval.

import { useEffect, useState } from 'preact/hooks'

import { asOgunError } from '../api'
import type { OgunError, SampleImportResult, SampleInfo } from '../api/types'
import { shortHash } from '../lib/format'
import { href, navigate, useAsync, useBackend } from '../lib/hooks'
import { lastImport, setLastImport } from '../lib/importNotice'
import { ErrorBox, Loading } from '../ui/components'

const KEY = 'ogun.studio.welcome'

/** Whether the person chose "Start empty". Browser storage may be unavailable; then the panel simply shows again. */
export function welcomeDismissed(): boolean {
  try {
    return window.localStorage.getItem(KEY) === 'dismissed'
  } catch {
    return false
  }
}

function dismissWelcome(): void {
  try {
    window.localStorage.setItem(KEY, 'dismissed')
  } catch {
    // Not remembered; harmless.
  }
}

const TITLES: Record<string, string> = {
  starter: 'Import the Getting started project',
  bikeshare: 'Import the bike-share sample project',
}

/** A list of the workflows an import stored, each with its short hash and, when it waits, a link to Approvals. */
export function ImportedList({ result }: { result: SampleImportResult }) {
  return (
    <ul class="imported" aria-label="Stored workflows">
      {result.workflows.map((w) => (
        <li key={w.name}>
          <a href={href('workflows', w.name)}>{w.name}</a>
          <span class="muted">
            v{w.version} · <span class="mono" title={w.workflow_hash}>{shortHash(w.workflow_hash)}</span>
            {w.created ? '' : ' · already stored'}
          </span>
          {w.approvals > 0 ? (
            <a href={href('approvals')}>
              waits for {w.approvals} approval{w.approvals === 1 ? '' : 's'}
            </a>
          ) : null}
        </li>
      ))}
    </ul>
  )
}

/** The sample projects, as buttons. `onImported` runs after a project is stored. */
export function SampleImport({ onImported, compact = false }: { onImported?: (r: SampleImportResult) => void; compact?: boolean }) {
  const backend = useBackend()
  const samples = useAsync(() => (backend.engine ? backend.engine.samples() : Promise.resolve([] as SampleInfo[])), [backend])
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<OgunError | undefined>()
  const [done, setDone] = useState<SampleImportResult | null>(null)
  const run = async (id: string) => {
    setBusy(id)
    setError(undefined)
    try {
      const r = await backend.engine?.importSample(id)
      if (r) {
        setDone(r)
        setLastImport(r)
        onImported?.(r)
      }
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(null)
    }
  }
  if (!backend.engine) return null
  return (
    <div>
      <ErrorBox error={samples.error} title="Samples" />
      <ErrorBox error={error} title="Import" />
      {samples.loading && !samples.data ? <Loading /> : null}
      <div class="choices">
        {(samples.data ?? []).map((s) => (
          <button type="button" class="choice" disabled={busy !== null} onClick={() => void run(s.id)} key={s.id}>
            <span class="name">{busy === s.id ? 'Importing…' : (TITLES[s.id] ?? `Import ${s.name}`)}</span>
            <span class="meta">{compact ? `${s.workflows.length} workflows${s.stored ? `, ${s.stored} already stored` : ''}` : s.description}</span>
            {compact ? null : (
              <span class="meta">
                {s.workflows.length} workflows{s.stored ? ` · ${s.stored} already stored` : ''}
              </span>
            )}
          </button>
        ))}
      </div>
      {done ? <ImportedList result={done} /> : null}
    </div>
  )
}

/** The first-run panel: shown on Overview while the engine holds no workflow and the person has not dismissed it. */
export function Welcome({ workflows }: { workflows: number }) {
  const backend = useBackend()
  const [hidden, setHidden] = useState(welcomeDismissed())
  useEffect(() => setHidden(welcomeDismissed()), [workflows])
  if (!backend.engine || workflows > 0 || hidden) return null
  return (
    <section class="welcome" aria-label="Welcome">
      <h2>Welcome to Ogun Studio</h2>
      <p>
        Studio runs its own engine on this computer. Workflows you store here run on this machine, on a schedule while Studio is open, and every result is kept with its hash and lineage. Nothing leaves this computer unless a workflow fetches from the internet or you add a provider key for the agent.
      </p>
      <p class="muted">Start with a sample project, or start empty and store your own.</p>
      <SampleImport
        onImported={() => {
          navigate(href('workflows'))
        }}
      />
      <div class="toolbar">
        <button
          type="button"
          class="btn"
          onClick={() => {
            dismissWelcome()
            setHidden(true)
          }}
        >
          Start empty
        </button>
      </div>
    </section>
  )
}

/** A banner on the Workflows page after an import: what was stored, and what waits for a person. */
export function ImportBanner() {
  const [r, setR] = useState<SampleImportResult | null>(lastImport())
  if (!r) return null
  const waiting = r.workflows.reduce((n, w) => n + w.approvals, 0)
  return (
    <div class="notice" role="status">
      <div>
        <strong>Imported {r.workflows.length} workflows</strong> from the {r.project} project.
        {waiting > 0 ? (
          <>
            {' '}
            {waiting} approval{waiting === 1 ? '' : 's'} wait for a person: <a href={href('approvals')}>open Approvals</a>.
          </>
        ) : null}
      </div>
      <ImportedList result={r} />
      <div class="toolbar">
        <button
          type="button"
          class="btn small"
          onClick={() => {
            setLastImport(null)
            setR(null)
          }}
        >
          Dismiss
        </button>
      </div>
    </div>
  )
}
