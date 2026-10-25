// The command palette (Ctrl+K): pages, settings sections, themes, stored workflows, recent runs, pending approvals,
// connectors and, on the demo backend, the scenario controls. Keyboard first: arrows move, Enter opens, Escape
// closes. The theme picker (Ctrl+Alt+A) opens it straight on the list of themes, as T3 Code does.

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import type { Backend } from '../api'
import { categoryLabel, useCatalog } from '../connectors'
import { useAgentThreads } from '../lib/agentHooks'
import { href, useAsync } from '../lib/hooks'
import { NAV } from '../lib/nav'
import { setPrefs, usePrefs } from '../lib/prefs'
import { allThemes, swatch } from '../lib/themes'
import { SETTINGS_SECTIONS } from '../views/settings'
import type { IconName } from './icons'
import { Icon } from './icons'

interface Item {
  group: string
  name: string
  meta?: string
  icon: IconName
  /** Two colours for a theme row. */
  balls?: [string, string]
  current?: boolean
  run: () => void
}

export function Palette({ backend, mode, onClose, go }: { backend: Backend; mode: 'all' | 'themes'; onClose: () => void; go: (to: string) => void }) {
  const prefs = usePrefs()
  const [q, setQ] = useState('')
  const [sel, setSel] = useState(0)
  const [view, setView] = useState<'all' | 'themes'>(mode)
  const input = useRef<HTMLInputElement>(null)
  const catalog = useCatalog()
  const threads = useAgentThreads()
  const workflows = useAsync(() => backend.workflowsList().catch(() => ({ workflows: [] })), [backend])
  const runs = useAsync(() => backend.runsList({ limit: 8 }).catch(() => ({ runs: [] })), [backend])
  const approvals = useAsync(() => backend.approvalsList('pending').catch(() => ({ approvals: [] })), [backend])
  useEffect(() => {
    input.current?.focus()
  }, [view])

  const items = useMemo<Item[]>(() => {
    const out: Item[] = []
    const open = (to: string) => () => {
      go(to)
      onClose()
    }
    if (view === 'themes') {
      for (const t of allThemes(prefs.customThemes)) {
        const l = swatch(t, 'light')
        const d = swatch(t, 'dark')
        out.push({ group: 'Change theme', name: t.label, icon: 'palette', balls: [l.primary, d.primary], current: t.id === prefs.theme, run: () => (setPrefs({ theme: t.id }), onClose()) })
      }
      const needle = q.trim().toLowerCase()
      return needle ? out.filter((i) => i.name.toLowerCase().includes(needle)) : out
    }
    if (backend.agent) out.push({ group: 'Agent', name: 'New thread', meta: 'Give the agent a task', icon: 'compose', run: open(href('agent')) })
    for (const t of (backend.agent ? threads : undefined) ?? []) out.push({ group: 'Agent threads', name: t.title, meta: t.state === 'idle' ? '' : t.state, icon: 'chat', run: open(href('agent', t.thread_id)) })
    for (const n of NAV.filter((x) => x.id !== 'gateway' && (x.id !== 'agent' || backend.agent))) out.push({ group: 'Pages', name: n.label, icon: n.icon, run: open(href(...(n.to ?? (n.id ? [n.id] : [])))) })
    out.push({ group: 'Pages', name: 'About Ogun Studio', icon: 'info', run: open(href('about')) })
    for (const s of SETTINGS_SECTIONS) out.push({ group: 'Settings', name: s.label, meta: s.hint, icon: s.icon, run: open(href('settings', s.id)) })
    out.push({ group: 'Appearance', name: 'Change theme', icon: 'palette', run: () => (setView('themes'), setQ('')) })
    for (const a of ['system', 'light', 'dark'] as const) out.push({ group: 'Appearance', name: `Appearance: ${a[0]?.toUpperCase()}${a.slice(1)}`, icon: a === 'dark' ? 'moon' : a === 'light' ? 'sun' : 'contrast', current: prefs.appearance === a, run: () => (setPrefs({ appearance: a }), onClose()) })
    for (const w of workflows.data?.workflows ?? []) out.push({ group: 'Workflows', name: w.name, meta: `v${w.latest_version}${w.runnable ? '' : ' · needs approval'}`, icon: 'dag', run: open(href('workflows', w.name)) })
    for (const a of approvals.data?.approvals ?? []) out.push({ group: 'Pending approvals', name: `${a.kind.replace('_', ' ')} · ${a.workflow}${a.node ? ' · ' + a.node : ''}`, meta: a.requested_by, icon: 'seal', run: open(href('approvals', a.approval_id)) })
    for (const r of runs.data?.runs ?? []) out.push({ group: 'Recent runs', name: r.run_id, meta: `${r.workflow} · ${r.state}`, icon: 'play', run: open(href('runs', r.run_id)) })
    if (backend.demo) {
      const d = backend.demo
      out.push({ group: 'Scenario', name: 'Kill ogund (SIGKILL) and restart it', meta: 'the O1 exit test', icon: 'bolt', run: () => (onClose(), d.restartDaemon()) })
      out.push({ group: 'Scenario', name: 'Deliver late station data', meta: 'a window revision', icon: 'wave', run: () => (onClose(), d.lateData()) })
      out.push({ group: 'Scenario', name: 'Reset the scenario', icon: 'refresh', run: () => (onClose(), d.reset()) })
    }
    const needle = q.trim().toLowerCase()
    const base = needle ? out.filter((i) => `${i.group} ${i.name} ${i.meta ?? ''}`.toLowerCase().includes(needle)) : out
    // Connectors are many: list them only once someone types.
    if (needle.length >= 2)
      for (const m of catalog ?? []) {
        const hay = `${m.name} ${m.vendor} ${m.id} ${m.operators.map((o) => o.op).join(' ')}`.toLowerCase()
        if (needle.split(/\s+/).every((w) => hay.includes(w))) base.push({ group: 'Connectors', name: m.name, meta: `${m.vendor} · ${categoryLabel(m.category)}`, icon: 'plug', run: open(href('settings', 'connectors', m.id)) })
      }
    return base.slice(0, 60)
  }, [q, view, prefs, workflows.data, runs.data, approvals.data, catalog, threads, backend, go, onClose])

  useEffect(() => setSel(0), [q, view])
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') onClose()
    else if (e.key === 'ArrowDown') {
      e.preventDefault()
      setSel((s) => Math.min(s + 1, items.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setSel((s) => Math.max(s - 1, 0))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      items[sel]?.run()
    } else if (e.key === 'Backspace' && !q && view === 'themes') {
      e.preventDefault()
      setView('all')
    }
  }
  let lastGroup = ''
  return (
    <div
      class="palette-back"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <div class="palette" role="dialog" aria-modal="true" aria-label="Search" onKeyDown={onKey}>
        <div class="search">
          {view === 'themes' ? (
            <button type="button" class="icon-btn small" aria-label="Back" onClick={() => setView('all')}>
              <Icon name="arrowleft" />
            </button>
          ) : (
            <Icon name="search" />
          )}
          <input ref={input} value={q} placeholder={view === 'themes' ? 'Search themes…' : 'Search pages, settings, workflows, runs, connectors…'} aria-label="Search" onInput={(e) => setQ((e.target as HTMLInputElement).value)} />
        </div>
        <div class="list" role="listbox">
          {items.length === 0 ? <div class="group">Nothing matches.</div> : null}
          {items.map((it, i) => {
            const head = it.group !== lastGroup ? <div class="group">{it.group}</div> : null
            lastGroup = it.group
            return (
              <>
                {head}
                <button type="button" class="item" role="option" aria-selected={i === sel} onMouseEnter={() => setSel(i)} onClick={it.run}>
                  <Icon name={it.icon} />
                  <span class="name">{it.name}</span>
                  {it.meta ? <span class="meta">{it.meta}</span> : null}
                  {it.current ? <span class="meta">Current</span> : null}
                  {it.balls ? (
                    <span class="balls tiny">
                      <i class="ball" style={{ background: it.balls[0] }} />
                      <i class="ball" style={{ background: it.balls[1] }} />
                    </span>
                  ) : null}
                </button>
              </>
            )
          })}
        </div>
        <div class="foot">
          <span>
            <kbd>↑</kbd>
            <kbd>↓</kbd> Navigate
          </span>
          <span>
            <kbd>Enter</kbd> Select
          </span>
          {view === 'themes' ? (
            <span>
              <kbd>Backspace</kbd> Back
            </span>
          ) : null}
          <span>
            <kbd>Esc</kbd> Close
          </span>
        </div>
      </div>
    </div>
  )
}
