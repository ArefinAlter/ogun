import { useEffect, useState } from 'preact/hooks'

import { COMMANDS, WHENS, bindingOf, comboFromEvent, conflicts, formatCombo, fromEntries, setRecording, toEntries, usable, whenOf } from '../../lib/keys'
import type { When } from '../../lib/keys'
import { setPrefs, usePrefs } from '../../lib/prefs'
import { CopyButton, Dialog, PageHead } from '../../ui/components'
import { Icon } from '../../ui/icons'

function Combo({ combo }: { combo: string }) {
  const parts = formatCombo(combo)
  if (!parts.length) return <span class="muted">Unbound</span>
  return (
    <span class="combo">
      {parts.map((k) => (
        <kbd>{k}</kbd>
      ))}
    </span>
  )
}

function JsonEditor({ onClose }: { onClose: () => void }) {
  const p = usePrefs()
  const [text, setText] = useState(() => JSON.stringify(toEntries(p), null, 2))
  const [error, setError] = useState('')
  const save = () => {
    const r = fromEntries(text)
    if (!r.ok) return setError(r.error)
    setPrefs({ keybindings: r.keybindings, keyWhen: r.keyWhen })
    onClose()
  }
  return (
    <Dialog title="Edit keybindings as JSON" onClose={onClose}>
      <p class="muted">
        One entry per command: <span class="mono">command</span>, <span class="mono">key</span> (<span class="mono">mod</span> is Ctrl, or Command on a Mac; <span class="mono">""</span> unbinds) and <span class="mono">when</span> (<span class="mono">always</span>, <span class="mono">!inputFocus</span> or <span class="mono">!modalOpen</span>).
      </p>
      <textarea aria-label="Keybindings JSON" spellcheck={false} value={text} style={{ minHeight: '320px' }} onInput={(e) => setText((e.target as HTMLTextAreaElement).value)} />
      {error ? (
        <div class="notice error" role="alert" style={{ marginTop: '8px' }}>
          {error}
        </div>
      ) : null}
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Cancel
        </button>
        <button type="button" class="btn primary" onClick={save}>
          Save keybindings
        </button>
      </div>
    </Dialog>
  )
}

export function KeybindingSettings() {
  const p = usePrefs()
  const [query, setQuery] = useState('')
  const [recording, setRec] = useState<string | null>(null)
  const [message, setMessage] = useState('')
  const [json, setJson] = useState(false)
  const [adding, setAdding] = useState(false)

  useEffect(() => {
    if (!recording) return undefined
    setRecording(true)
    const on = (e: KeyboardEvent) => {
      e.preventDefault()
      e.stopPropagation()
      if (e.key === 'Escape') {
        setRec(null)
        setMessage('')
        return
      }
      if ((e.key === 'Backspace' || e.key === 'Delete') && !e.ctrlKey && !e.altKey && !e.metaKey) {
        setPrefs({ keybindings: { ...p.keybindings, [recording]: '' } })
        setRec(null)
        setMessage('')
        return
      }
      const combo = comboFromEvent(e)
      if (!combo) return
      if (!usable(combo)) {
        setMessage('Add a modifier key (Ctrl, Alt or Shift), or use a function key.')
        return
      }
      const clash = conflicts(p, combo, recording)[0]
      if (clash) {
        setMessage(`${formatCombo(combo).join('+')} is already used by "${clash.label}". Unbind that one first.`)
        return
      }
      const def = COMMANDS.find((c) => c.id === recording)?.default ?? ''
      const next = { ...p.keybindings }
      if (combo === def) delete next[recording]
      else next[recording] = combo
      setPrefs({ keybindings: next })
      setRec(null)
      setMessage('')
    }
    window.addEventListener('keydown', on, true)
    return () => {
      window.removeEventListener('keydown', on, true)
      setRecording(false)
    }
  }, [recording, p])

  const q = query.trim().toLowerCase()
  const shown = COMMANDS.filter((c) => !q || `${c.label} ${c.group} ${c.id} ${formatCombo(bindingOf(p, c.id)).join(' ')}`.toLowerCase().includes(q))
  const groups = [...new Set(shown.map((c) => c.group))]
  const customised = Object.keys(p.keybindings).length + Object.keys(p.keyWhen).length
  const unbound = COMMANDS.filter((c) => !bindingOf(p, c.id))

  return (
    <>
      <PageHead title="Keybindings" sub="Click a shortcut, then press the keys you want. Escape cancels; Backspace unbinds. Stored on this device.">
        <button type="button" class="btn" disabled={!customised} onClick={() => setPrefs({ keybindings: {}, keyWhen: {} })}>
          Reset all{customised ? ` (${customised})` : ''}
        </button>
      </PageHead>
      <div class="kbbar">
        <label class="searchbox">
          <Icon name="search" />
          <input type="search" aria-label="Search keybindings" placeholder="Search keybindings" value={query} onInput={(e) => setQuery((e.target as HTMLInputElement).value)} />
        </label>
        <button type="button" class="btn" onClick={() => setAdding((v) => !v)} aria-expanded={adding}>
          <Icon name="plus" /> Add keybinding
        </button>
        <button type="button" class="icon-btn boxed" aria-label="Edit as JSON" title="Edit as JSON" onClick={() => setJson(true)}>
          <Icon name="code" />
        </button>
        <CopyButton text={JSON.stringify(toEntries(p), null, 2)} label="Copy JSON" />
      </div>
      {adding ? (
        <div class="notice">
          {unbound.length === 0 ? (
            'Every command already has a shortcut. Click one below to change it.'
          ) : (
            <>
              Unbound commands:{' '}
              {unbound.map((c) => (
                <button type="button" class="btn small" style={{ marginRight: '6px' }} onClick={() => (setRec(c.id), setAdding(false))}>
                  {c.label}
                </button>
              ))}
            </>
          )}
        </div>
      ) : null}
      {message ? (
        <div class="notice warn" role="status">
          {message}
        </div>
      ) : null}
      {groups.map((g) => (
        <section class="sgroup" key={g}>
          <h2 class="sgroup-title">{g}</h2>
          <div class="rows">
            {shown
              .filter((c) => c.group === g)
              .map((c) => {
                const combo = bindingOf(p, c.id)
                const changed = p.keybindings[c.id] !== undefined || p.keyWhen[c.id] !== undefined
                const isRec = recording === c.id
                return (
                  <div class="row" key={c.id}>
                    <div class="text">
                      <div class="title">{c.label}</div>
                      <div class="desc">
                        When{' '}
                        <select class="when" aria-label={`When ${c.label} fires`} value={whenOf(p, c.id)} onChange={(e) => {
                          const v = (e.target as HTMLSelectElement).value as When
                          const next = { ...p.keyWhen }
                          if (v === c.when) delete next[c.id]
                          else next[c.id] = v
                          setPrefs({ keyWhen: next })
                        }}>
                          {WHENS.map(([id, label]) => (
                            <option value={id}>{label}</option>
                          ))}
                        </select>
                      </div>
                    </div>
                    <div class="control">
                      <button
                        type="button"
                        class={`keybtn ${isRec ? 'recording' : ''}`}
                        aria-label={`Shortcut for ${c.label}. Click to change.`}
                        onClick={() => {
                          setMessage('')
                          setRec(isRec ? null : c.id)
                        }}
                      >
                        {isRec ? <span class="muted">Press the new shortcut…</span> : <Combo combo={combo} />}
                      </button>
                      {changed ? (
                        <button
                          type="button"
                          class="btn small ghost"
                          onClick={() => {
                            const kb = { ...p.keybindings }
                            const kw = { ...p.keyWhen }
                            delete kb[c.id]
                            delete kw[c.id]
                            setPrefs({ keybindings: kb, keyWhen: kw })
                          }}
                        >
                          Reset
                        </button>
                      ) : null}
                    </div>
                  </div>
                )
              })}
          </div>
        </section>
      ))}
      {!shown.length ? <div class="empty">No command matches.</div> : null}
      {json ? <JsonEditor onClose={() => setJson(false)} /> : null}
    </>
  )
}
