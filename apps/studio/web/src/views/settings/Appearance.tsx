import { useState } from 'preact/hooks'

import type { Appearance, ContentWidth, CustomTheme, Density, StatusColors } from '../../lib/prefs'
import { setPrefs, usePrefs } from '../../lib/prefs'
import type { Scheme, Theme } from '../../lib/themes'
import { THEMES, allThemes, previewTokens, swatch, systemScheme } from '../../lib/themes'
import { CopyButton, Dialog, PageHead, SettingRow, SettingsGroup, Slider, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'

/** A miniature of the app in one scheme: sidebar, a rule, a card, the primary role. */
function MiniApp({ theme, scheme }: { theme: Theme; scheme: Scheme }) {
  const t = previewTokens(theme, scheme)
  const bg = t['--background']
  const side = t['--sidebar']
  const card = t['--card']
  const line = t['--border']
  const primary = t['--primary']
  const fg = t['--foreground']
  return (
    <div class="mini" style={{ background: bg, borderColor: line }} aria-hidden="true">
      <div class="mini-side" style={{ background: side, borderColor: line }}>
        <i style={{ background: card, borderColor: line }} />
        <i style={{ background: line }} />
        <i style={{ background: line }} />
      </div>
      <div class="mini-main">
        <b style={{ background: fg, opacity: 0.55 }} />
        <b style={{ background: fg, opacity: 0.25, width: '46%' }} />
        <div class="mini-card" style={{ background: card, borderColor: line }}>
          <b style={{ background: line }} />
          <b style={{ background: line, width: '60%' }} />
          <em style={{ background: primary }} />
        </div>
      </div>
    </div>
  )
}

function SchemeCard({ id, label, selected, theme, onPick }: { id: Appearance; label: string; selected: boolean; theme: Theme; onPick: () => void }) {
  return (
    <button type="button" class={`scheme-card ${selected ? 'on' : ''}`} aria-pressed={selected} onClick={onPick}>
      {id === 'system' ? (
        <div class="split">
          <div class="half l">
            <MiniApp theme={theme} scheme="light" />
          </div>
          <div class="half d">
            <MiniApp theme={theme} scheme="dark" />
          </div>
        </div>
      ) : (
        <MiniApp theme={theme} scheme={id} />
      )}
      <span class="scheme-label">{label}</span>
    </button>
  )
}

function slug(label: string, taken: Set<string>): string {
  const base = label.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 24) || 'theme'
  let id = base
  let n = 2
  while (taken.has(id)) id = `${base}-${n++}`
  return id
}

function CreateTheme({ initial, onClose }: { initial?: CustomTheme; onClose: () => void }) {
  const p = usePrefs()
  const [label, setLabel] = useState(initial?.label ?? '')
  const [hue, setHue] = useState(initial?.hue ?? 200)
  const [tint, setTint] = useState(initial?.tint ?? 60)
  const [accent, setAccent] = useState(initial?.accent ?? '#2563eb')
  const draft: CustomTheme = { id: initial?.id ?? 'draft', label: label || 'Untitled', hue, tint, accent }
  const preview = allThemes([draft]).find((t) => t.id === draft.id) as Theme
  const save = () => {
    const taken = new Set([...THEMES.map((t) => t.id), ...p.customThemes.map((c) => c.id)])
    const id = initial && p.customThemes.some((c) => c.id === initial.id) ? initial.id : slug(label, taken)
    const next = [...p.customThemes.filter((c) => c.id !== id), { ...draft, id, label: label.trim() || 'Untitled' }]
    setPrefs({ customThemes: next, theme: id })
    onClose()
  }
  return (
    <Dialog title={initial ? 'Edit theme' : 'Create theme'} onClose={onClose}>
      <div class="theme-editor">
        <div class="pair">
          <MiniApp theme={preview} scheme="light" />
          <MiniApp theme={preview} scheme="dark" />
        </div>
        <label class="field">
          Name
          <input value={label} maxLength={32} placeholder="Harbour" onInput={(e) => setLabel((e.target as HTMLInputElement).value)} />
        </label>
        <label class="field">
          Hue
          <Slider value={hue} min={0} max={360} unit="°" label="Hue" onChange={setHue} />
        </label>
        <label class="field">
          How strongly surfaces carry the hue
          <Slider value={tint} min={0} max={100} unit="%" label="Tint" onChange={setTint} />
        </label>
        <label class="field">
          Accent
          <input type="color" aria-label="Accent" value={accent} onInput={(e) => setAccent((e.target as HTMLInputElement).value)} />
        </label>
      </div>
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Cancel
        </button>
        <button type="button" class="btn primary" disabled={!label.trim()} onClick={save}>
          Save theme
        </button>
      </div>
    </Dialog>
  )
}

function ImportTheme({ onClose }: { onClose: () => void }) {
  const p = usePrefs()
  const [text, setText] = useState('')
  const [error, setError] = useState('')
  const add = () => {
    try {
      const v = JSON.parse(text) as Partial<CustomTheme>
      if (typeof v.label !== 'string' || typeof v.hue !== 'number' || typeof v.tint !== 'number' || typeof v.accent !== 'string' || !/^#[0-9a-fA-F]{6}$/.test(v.accent)) throw new Error('A theme has label, hue (0 to 360), tint (0 to 100) and accent (#rrggbb).')
      const taken = new Set([...THEMES.map((t) => t.id), ...p.customThemes.map((c) => c.id)])
      const id = slug(v.label, taken)
      setPrefs({ customThemes: [...p.customThemes, { id, label: v.label, hue: v.hue, tint: v.tint, accent: v.accent.toLowerCase() }], theme: id })
      onClose()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Not valid JSON.')
    }
  }
  return (
    <Dialog title="Add a theme" onClose={onClose}>
      <p class="muted">Paste a theme exported from another copy of Studio. It adds colours only; it cannot run anything.</p>
      <textarea aria-label="Theme JSON" spellcheck={false} value={text} placeholder='{"label":"Harbour","hue":220,"tint":60,"accent":"#2563eb"}' onInput={(e) => setText((e.target as HTMLTextAreaElement).value)} />
      {error ? (
        <div class="notice error" role="alert" style={{ marginTop: '8px' }}>
          {error}
        </div>
      ) : null}
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Cancel
        </button>
        <button type="button" class="btn primary" disabled={!text.trim()} onClick={add}>
          Add theme
        </button>
      </div>
    </Dialog>
  )
}

function ThemeCard({ theme, custom, selected, onEdit }: { theme: Theme; custom?: CustomTheme; selected: boolean; onEdit: (c: CustomTheme | null, duplicate?: boolean) => void }) {
  const p = usePrefs()
  const l = swatch(theme, 'light')
  const d = swatch(theme, 'dark')
  const ball = (c: { background: string; primary: string }) => ({ background: `radial-gradient(circle at 30% 28%, ${c.primary}, ${c.background} 78%)` })
  const base: CustomTheme = custom ?? { id: theme.id, label: theme.label, hue: 200, tint: 50, accent: l.primary.startsWith('#') ? l.primary : '#2563eb' }
  return (
    <div class={`theme-card ${selected ? 'on' : ''}`}>
      <button type="button" class="theme-pick" aria-pressed={selected} onClick={() => setPrefs({ theme: theme.id })}>
        <span class="balls">
          <i class="ball" style={ball(l)} />
          <i class="ball" style={ball(d)} />
        </span>
        <span class="theme-name">{theme.label}</span>
        <span class="theme-note">{theme.note}</span>
      </button>
      <span class="theme-tools">
        <button type="button" class="icon-btn small" aria-label={`Duplicate ${theme.label}`} title="Duplicate as a new theme" onClick={() => onEdit({ ...base, id: 'draft', label: `${theme.label} copy` }, true)}>
          <Icon name="copy" />
        </button>
        {custom ? (
          <>
            <button type="button" class="icon-btn small" aria-label={`Edit ${theme.label}`} title="Edit" onClick={() => onEdit(custom)}>
              <Icon name="sliders" />
            </button>
            <button
              type="button"
              class="icon-btn small"
              aria-label={`Delete ${theme.label}`}
              title="Delete"
              onClick={() => setPrefs({ customThemes: p.customThemes.filter((c) => c.id !== custom.id), ...(p.theme === custom.id ? { theme: 'ogun' } : {}) })}
            >
              <Icon name="trash" />
            </button>
          </>
        ) : null}
      </span>
    </div>
  )
}

export function AppearanceSettings() {
  const p = usePrefs()
  const [editor, setEditor] = useState<{ initial?: CustomTheme } | null>(null)
  const [importing, setImporting] = useState(false)
  const themes = allThemes(p.customThemes)
  const theme = themes.find((t) => t.id === p.theme) ?? (THEMES[0] as Theme)
  const sys = systemScheme()
  const current = p.customThemes.find((c) => c.id === p.theme)
  return (
    <>
      <PageHead title="Appearance" sub="Stored on this device only. Nothing here is sent to a daemon or a server." />
      <SettingsGroup title="Color scheme">
        <div class="scheme-grid">
          <SchemeCard id="system" label={`System (${sys})`} selected={p.appearance === 'system'} theme={theme} onPick={() => setPrefs({ appearance: 'system' })} />
          <SchemeCard id="light" label="Light" selected={p.appearance === 'light'} theme={theme} onPick={() => setPrefs({ appearance: 'light' })} />
          <SchemeCard id="dark" label="Dark" selected={p.appearance === 'dark'} theme={theme} onPick={() => setPrefs({ appearance: 'dark' })} />
        </div>
      </SettingsGroup>
      <section class="sgroup">
        <div class="sgroup-head">
          <h2 class="sgroup-title">Themes</h2>
          <div class="sgroup-actions">
            <button type="button" class="btn small" onClick={() => setEditor({})}>
              <Icon name="palette" /> Create theme
            </button>
            <button type="button" class="btn small" onClick={() => setImporting(true)}>
              <Icon name="plus" /> Add theme
            </button>
          </div>
        </div>
        <div class="rows">
          <div class="theme-grid">
            {themes.map((t) => {
              const custom = p.customThemes.find((c) => c.id === t.id)
              return <ThemeCard theme={t} {...(custom ? { custom } : {})} selected={t.id === p.theme} key={t.id} onEdit={(c, dup) => setEditor({ ...(c ? { initial: dup ? { ...c } : c } : {}) })} />
            })}
          </div>
        </div>
        <p class="sgroup-note">
          Each theme has a light and a dark half; the scheme above picks which one you see. Ctrl+Alt+A opens the theme picker and Ctrl+Alt+Shift+A cycles the scheme. {current ? <CopyButton text={JSON.stringify({ label: current.label, hue: current.hue, tint: current.tint, accent: current.accent })} label="Copy this theme's JSON" /> : null}
        </p>
      </section>
      <SettingsGroup title="Interface">
        <SettingRow title="Contrast" desc="The strength of borders and rules across the interface.">
          <Slider value={p.contrast} min={50} max={150} step={5} unit="%" label="Contrast" onChange={(v) => setPrefs({ contrast: v })} />
        </SettingRow>
        <SettingRow title="Glass opacity" desc="Higher values make menus, dialogs and the command palette more solid.">
          <Slider value={p.glass} min={50} max={100} step={5} unit="%" label="Glass opacity" onChange={(v) => setPrefs({ glass: v })} />
        </SettingRow>
        <SettingRow title="Density" desc="Row height in tables and lists, and the height of controls.">
          <select aria-label="Density" value={p.density} onChange={(e) => setPrefs({ density: (e.target as HTMLSelectElement).value as Density })}>
            <option value="comfortable">Comfortable</option>
            <option value="compact">Compact</option>
          </select>
        </SettingRow>
        <SettingRow title="Content width" desc="How wide pages may grow on large screens.">
          <select aria-label="Content width" value={p.contentWidth} onChange={(e) => setPrefs({ contentWidth: (e.target as HTMLSelectElement).value as ContentWidth })}>
            <option value="comfortable">Comfortable</option>
            <option value="wide">Wide</option>
            <option value="full">Full width</option>
          </select>
        </SettingRow>
        <SettingRow title="Status colors" desc="Succeeded and failed are always printed as words; this changes the colors that accompany them.">
          <select aria-label="Status colors" value={p.statusColors} onChange={(e) => setPrefs({ statusColors: (e.target as HTMLSelectElement).value as StatusColors })}>
            <option value="standard">Green and red</option>
            <option value="colorblind">Blue and orange</option>
          </select>
        </SettingRow>
        <SettingRow title="Reduce motion" desc="Turns off transitions. Also follows your operating system's setting.">
          <Switch label="Reduce motion" checked={p.reduceMotion} onChange={(v) => setPrefs({ reduceMotion: v })} />
        </SettingRow>
        <SettingRow title="Stored workflows in the sidebar" desc="Lists each stored workflow under the sections, with its version and age.">
          <Switch label="Stored workflows in the sidebar" checked={p.sidebarWorkflows} onChange={(v) => setPrefs({ sidebarWorkflows: v })} />
        </SettingRow>
      </SettingsGroup>
      {editor ? <CreateTheme {...(editor.initial ? { initial: editor.initial } : {})} onClose={() => setEditor(null)} /> : null}
      {importing ? <ImportTheme onClose={() => setImporting(false)} /> : null}
    </>
  )
}
