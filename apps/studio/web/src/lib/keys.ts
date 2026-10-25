// Keyboard shortcuts: the command list, matching, recording and display. A binding is a combo string such as
// `mod+k` or `ctrl+alt+shift+a`: lower case, modifiers in the fixed order mod/ctrl, alt, shift, then one key.
// `mod` means Ctrl on Windows and Linux and Command on macOS. Overrides live in the preferences; the defaults
// below follow T3 Code where it has the same command (Ctrl+K, Ctrl+comma, Ctrl+Alt+A, Ctrl+Alt+Shift+A).

import type { Prefs } from './prefs'

/** Where a shortcut fires: everywhere, only when no text field has focus, or only when no dialog is open. */
export type When = 'always' | '!inputFocus' | '!modalOpen'

export const WHENS: [When, string][] = [
  ['always', 'Always'],
  ['!inputFocus', 'Not while typing'],
  ['!modalOpen', 'No dialog open'],
]

export interface KeyCommand {
  id: string
  label: string
  group: string
  /** The default combo; `''` = unbound by default. */
  default: string
  /** The default context. */
  when: When
}

export const COMMANDS: KeyCommand[] = [
  { id: 'palette.toggle', label: 'Open the command palette', group: 'General', default: 'mod+k', when: 'always' },
  { id: 'settings.open', label: 'Open settings', group: 'General', default: 'mod+comma', when: 'always' },
  { id: 'sidebar.toggle', label: 'Show or hide the sidebar', group: 'General', default: 'mod+b', when: '!inputFocus' },
  { id: 'theme.pick', label: 'Change theme', group: 'Appearance', default: 'mod+alt+a', when: 'always' },
  { id: 'appearance.cycle', label: 'Cycle appearance: system, light, dark', group: 'Appearance', default: 'mod+alt+shift+a', when: 'always' },
  { id: 'agent.new', label: 'New agent thread', group: 'Agent', default: 'mod+shift+o', when: '!modalOpen' },
  { id: 'nav.agent', label: 'Go to Agent', group: 'Navigate', default: 'alt+0', when: '!modalOpen' },
  { id: 'nav.overview', label: 'Go to Overview', group: 'Navigate', default: 'alt+1', when: '!modalOpen' },
  { id: 'nav.workflows', label: 'Go to Workflows', group: 'Navigate', default: 'alt+2', when: '!modalOpen' },
  { id: 'nav.runs', label: 'Go to Runs', group: 'Navigate', default: 'alt+3', when: '!modalOpen' },
  { id: 'nav.partitions', label: 'Go to Partitions', group: 'Navigate', default: 'alt+4', when: '!modalOpen' },
  { id: 'nav.approvals', label: 'Go to Approvals', group: 'Navigate', default: 'alt+5', when: '!modalOpen' },
  { id: 'nav.artifacts', label: 'Go to Artifacts and lineage', group: 'Navigate', default: 'alt+6', when: '!modalOpen' },
  { id: 'nav.streams', label: 'Go to Streams', group: 'Navigate', default: 'alt+7', when: '!modalOpen' },
  { id: 'nav.edges', label: 'Go to Edges', group: 'Navigate', default: 'alt+8', when: '!modalOpen' },
  { id: 'nav.connectors', label: 'Go to Connectors', group: 'Navigate', default: 'alt+9', when: '!modalOpen' },
]

export function isMac(): boolean {
  return typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform)
}

/** The combo currently bound to a command (an override wins, `''` means unbound). */
export function bindingOf(prefs: Pick<Prefs, 'keybindings'>, id: string): string {
  const o = prefs.keybindings[id]
  if (o !== undefined) return o
  return COMMANDS.find((c) => c.id === id)?.default ?? ''
}

const NAMED: Record<string, string> = {
  ',': 'comma',
  '.': 'period',
  '/': 'slash',
  ';': 'semicolon',
  "'": 'quote',
  '[': 'bracketleft',
  ']': 'bracketright',
  '\\': 'backslash',
  '-': 'minus',
  '=': 'equal',
  '`': 'backquote',
  ' ': 'space',
  arrowup: 'up',
  arrowdown: 'down',
  arrowleft: 'left',
  arrowright: 'right',
  esc: 'escape',
}

const MODIFIER_KEYS = new Set(['control', 'shift', 'alt', 'meta', 'altgraph', 'os'])

/** The key of an event as the combo grammar names it: letters and digits from `code` so Alt and Option do not change them. */
export function keyOf(e: KeyboardEvent): string | null {
  const k = e.key.toLowerCase()
  if (MODIFIER_KEYS.has(k)) return null
  if (/^Key[A-Z]$/.test(e.code)) return e.code.slice(3).toLowerCase()
  if (/^Digit[0-9]$/.test(e.code)) return e.code.slice(5)
  return NAMED[k] ?? (k.length === 1 ? k : /^[a-z0-9]+$/.test(k) ? k : null)
}

/** The combo an event represents, or null for a bare modifier press. */
export function comboFromEvent(e: KeyboardEvent, mac = isMac()): string | null {
  const key = keyOf(e)
  if (!key) return null
  const parts: string[] = []
  const ctrl = mac ? e.metaKey : e.ctrlKey
  if (ctrl) parts.push('mod')
  if (mac && e.ctrlKey) parts.push('ctrl')
  if (e.altKey) parts.push('alt')
  if (e.shiftKey) parts.push('shift')
  parts.push(key)
  return parts.join('+')
}

/** Display parts for a combo: `['Ctrl', 'Shift', 'K']`, or `['⌘', '⇧', 'K']` on macOS. */
export function formatCombo(combo: string, mac = isMac()): string[] {
  if (!combo) return []
  const names: Record<string, string> = {
    comma: ',',
    period: '.',
    slash: '/',
    semicolon: ';',
    quote: "'",
    bracketleft: '[',
    bracketright: ']',
    backslash: '\\',
    minus: '-',
    equal: '=',
    backquote: '`',
    space: 'Space',
    escape: 'Esc',
    up: '↑',
    down: '↓',
    left: '←',
    right: '→',
    enter: 'Enter',
    tab: 'Tab',
    backspace: 'Backspace',
    delete: 'Del',
  }
  return combo.split('+').map((p) => {
    if (p === 'mod') return mac ? '⌘' : 'Ctrl'
    if (p === 'ctrl') return mac ? '⌃' : 'Ctrl'
    if (p === 'alt') return mac ? '⌥' : 'Alt'
    if (p === 'shift') return mac ? '⇧' : 'Shift'
    return names[p] ?? (p.length === 1 ? p.toUpperCase() : p.charAt(0).toUpperCase() + p.slice(1))
  })
}

/** Commands (other than `except`) that already use a combo. */
export function conflicts(prefs: Pick<Prefs, 'keybindings'>, combo: string, except?: string): KeyCommand[] {
  if (!combo) return []
  return COMMANDS.filter((c) => c.id !== except && bindingOf(prefs, c.id) === combo)
}

/** A combo is acceptable when it has a modifier, or is a function key or an arrow or Escape-free key like `/`. */
export function usable(combo: string): boolean {
  const parts = combo.split('+')
  const key = parts[parts.length - 1] ?? ''
  const hasMod = parts.length > 1
  if (key === 'escape') return false
  return hasMod || /^f([1-9]|1[0-2])$/.test(key) || key === 'slash'
}

/** The context of a command under these preferences. */
export function whenOf(prefs: Pick<Prefs, 'keyWhen'>, id: string): When {
  const o = prefs.keyWhen[id]
  if (o === 'always' || o === '!inputFocus' || o === '!modalOpen') return o
  return COMMANDS.find((c) => c.id === id)?.when ?? 'always'
}

function contextOk(when: When): boolean {
  if (when === '!inputFocus') {
    const el = document.activeElement
    return !(el instanceof HTMLElement && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)))
  }
  if (when === '!modalOpen') return !document.querySelector('[aria-modal="true"]')
  return true
}

export interface KeyEntry {
  command: string
  key: string
  when: When
}

/** Every command with its current key and context: what the JSON editor shows. */
export function toEntries(prefs: Pick<Prefs, 'keybindings' | 'keyWhen'>): KeyEntry[] {
  return COMMANDS.map((c) => ({ command: c.id, key: bindingOf(prefs, c.id), when: whenOf(prefs, c.id) }))
}

/** Parses the JSON editor's text into overrides, or says what is wrong. */
export function fromEntries(text: string): { ok: true; keybindings: Record<string, string>; keyWhen: Record<string, string> } | { ok: false; error: string } {
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch (e) {
    return { ok: false, error: `Not valid JSON: ${e instanceof Error ? e.message : 'parse error'}` }
  }
  if (!Array.isArray(parsed)) return { ok: false, error: 'Expected an array of {command, key, when} entries.' }
  const keybindings: Record<string, string> = {}
  const keyWhen: Record<string, string> = {}
  const used = new Map<string, string>()
  for (const [i, raw] of parsed.entries()) {
    if (!raw || typeof raw !== 'object') return { ok: false, error: `Entry ${i + 1} is not an object.` }
    const e = raw as { command?: unknown; key?: unknown; when?: unknown }
    const cmd = COMMANDS.find((c) => c.id === e.command)
    if (!cmd) return { ok: false, error: `Entry ${i + 1}: unknown command ${JSON.stringify(e.command)}.` }
    if (typeof e.key !== 'string') return { ok: false, error: `${cmd.id}: key is a string such as "mod+k", or "" to unbind.` }
    const key = e.key.trim().toLowerCase()
    if (key && !usable(key)) return { ok: false, error: `${cmd.id}: "${key}" needs a modifier key.` }
    if (key) {
      const clash = used.get(key)
      if (clash) return { ok: false, error: `${cmd.id} and ${clash} both use "${key}".` }
      used.set(key, cmd.id)
    }
    if (key !== cmd.default) keybindings[cmd.id] = key
    const when = e.when ?? cmd.when
    if (when !== 'always' && when !== '!inputFocus' && when !== '!modalOpen') return { ok: false, error: `${cmd.id}: when is "always", "!inputFocus" or "!modalOpen".` }
    if (when !== cmd.when) keyWhen[cmd.id] = when
  }
  return { ok: true, keybindings, keyWhen }
}

let recording = false

/** While the Keybindings page records a combo, global shortcuts stand down. */
export function setRecording(on: boolean): void {
  recording = on
}

export function isRecording(): boolean {
  return recording
}

/** The command a keydown triggers under these preferences, if any. */
export function commandFor(e: KeyboardEvent, prefs: Pick<Prefs, 'keybindings' | 'keyWhen'>): KeyCommand | null {
  if (recording) return null
  const combo = comboFromEvent(e)
  if (!combo) return null
  const c = COMMANDS.find((x) => bindingOf(prefs, x.id) === combo)
  return c && contextOk(whenOf(prefs, c.id)) ? c : null
}
