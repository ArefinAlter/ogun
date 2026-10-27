// A masked field that stores one secret in the engine's secret store (desktop only). The value lives in the input
// until Save, is sent once to the engine through the shell, and is cleared. A secret is referred to everywhere
// else by its name; no page ever shows, returns or keeps a value.

import { useState } from 'preact/hooks'

import { asOgunError } from '../api'
import type { OgunError } from '../api/types'
import { useBackend } from '../lib/hooks'
import { Chip, ErrorBox } from './components'

export function SecretEntry({ name, stored, onChanged }: { name: string; stored: boolean; onChanged: () => void }) {
  const backend = useBackend()
  const [value, setValue] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  if (!backend.engine) return null
  const save = async () => {
    const v = value
    setValue('')
    setBusy(true)
    setError(undefined)
    try {
      await backend.engine?.putSecret(name, v)
      onChanged()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const remove = async () => {
    setBusy(true)
    setError(undefined)
    try {
      await backend.engine?.deleteSecret(name)
      onChanged()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <div class="stack-tight">
      <div class="keyrow">
        <input type="password" autocomplete="off" spellcheck={false} aria-label={`Value of the secret ${name}`} placeholder={stored ? 'Paste a new value to replace the stored one' : 'Paste the value'} value={value} onInput={(e) => setValue((e.target as HTMLInputElement).value)} />
        <button type="button" class="btn small primary" disabled={busy || value.length === 0} onClick={() => void save()}>
          Store the value
        </button>
        {stored ? (
          <>
            <Chip tone="ok">stored</Chip>
            <button type="button" class="btn small ghost" disabled={busy} onClick={() => void remove()}>
              Remove
            </button>
          </>
        ) : null}
      </div>
      <div class="muted small-text">Kept by the engine in this computer's credential store under the name <span class="mono">{name}</span>. It is never shown again or written to a log.</div>
      <ErrorBox error={error} title="Secret" />
    </div>
  )
}
