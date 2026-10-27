// Device preferences: how Studio looks and behaves on this machine. Stored in this webview's own storage (the
// shell's data directory on desktop, the origin's storage on the web), never sent anywhere and never holding a
// secret. Storage may be unavailable (private windows, blocked site data): every access is guarded and the
// defaults apply.

import { useEffect, useState } from 'preact/hooks'

export type Appearance = 'system' | 'light' | 'dark'
export type Density = 'comfortable' | 'compact'
export type ContentWidth = 'comfortable' | 'wide' | 'full'
export type StatusColors = 'standard' | 'colorblind'
export type Notifications = 'all' | 'important' | 'off'
export type TimeMode = 'utc' | 'local'

export interface Prefs {
  appearance: Appearance
  theme: string
  /** 50 to 150, 100 = as designed. Scales the strength of borders and rules. */
  contrast: number
  density: Density
  contentWidth: ContentWidth
  statusColors: StatusColors
  reduceMotion: boolean
  startPage: string
  timeMode: TimeMode
  confirmCancel: boolean
  notifications: Notifications
  liveUpdates: boolean
  sidebarWorkflows: boolean
  sidebarOpen: boolean
  /** The side panel of an agent thread. */
  agentDetails: boolean
  /** Command id -> combo, only for commands the person rebound (`''` = unbound). */
  keybindings: Record<string, string>
  /** Command id -> the context it fires in, only where the person changed it. */
  keyWhen: Record<string, string>
  /** How solid menus, dialogs and the palette are, 50 to 100 percent. */
  glass: number
  /** Themes the person made: a name, a hue, how strongly surfaces carry it, and the accent. */
  customThemes: CustomTheme[]
}

export interface CustomTheme {
  id: string
  label: string
  /** 0 to 360. */
  hue: number
  /** 0 to 100: how strongly surfaces carry the hue. */
  tint: number
  /** `#rrggbb` for the accent in the light scheme; the dark scheme derives from it. */
  accent: string
}

/** The desktop app opens on Overview, where a new workspace offers its sample projects; the agent needs a provider
 *  key before it can answer, so it is not the first thing a person meets. A browser keeps the Agent as the start page. */
const DESKTOP = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window && import.meta.env.VITE_OGUN_DESKTOP_DEMO !== '1'

export const DEFAULTS: Prefs = {
  appearance: 'system',
  theme: 'ogun',
  contrast: 100,
  density: 'comfortable',
  contentWidth: 'comfortable',
  statusColors: 'standard',
  reduceMotion: false,
  startPage: DESKTOP ? '' : 'agent',
  timeMode: 'utc',
  confirmCancel: true,
  notifications: 'important',
  liveUpdates: true,
  sidebarWorkflows: true,
  sidebarOpen: true,
  agentDetails: false,
  keybindings: {},
  keyWhen: {},
  glass: 92,
  customThemes: [],
}

const KEY = 'ogun-studio:prefs:v1'

/** The hosted web demo is built with `VITE_OGUN_NO_STORAGE=1`: it keeps preferences in memory only, so the site
 *  that serves it can keep its "no cookies, no local storage" promise. The desktop app and the daemon-served
 *  Studio keep them on the device. */
const PERSIST = import.meta.env.VITE_OGUN_NO_STORAGE !== '1'

function read(): Prefs {
  if (!PERSIST) return { ...DEFAULTS }
  try {
    const raw = window.localStorage.getItem(KEY)
    if (!raw) return { ...DEFAULTS }
    const parsed = JSON.parse(raw) as Partial<Prefs>
    return sanitize(parsed)
  } catch {
    return { ...DEFAULTS }
  }
}

function pick<T extends string>(v: unknown, allowed: readonly T[], fallback: T): T {
  return typeof v === 'string' && (allowed as readonly string[]).includes(v) ? (v as T) : fallback
}

function bool(v: unknown, fallback: boolean): boolean {
  return typeof v === 'boolean' ? v : fallback
}

/** Whatever was stored, as a complete and valid `Prefs`: unknown members and wrong types fall back to the defaults. */
export function sanitize(p: Partial<Prefs>): Prefs {
  const kb: Record<string, string> = {}
  if (p.keybindings && typeof p.keybindings === 'object')
    for (const [k, v] of Object.entries(p.keybindings)) if (typeof v === 'string' && v.length <= 40 && /^[a-z0-9_.-]+$/.test(k)) kb[k] = v
  const kw: Record<string, string> = {}
  if (p.keyWhen && typeof p.keyWhen === 'object')
    for (const [k, v] of Object.entries(p.keyWhen)) if (typeof v === 'string' && ['always', '!inputFocus', '!modalOpen'].includes(v) && /^[a-z0-9_.-]+$/.test(k)) kw[k] = v
  const custom: CustomTheme[] = []
  if (Array.isArray(p.customThemes))
    for (const t of p.customThemes.slice(0, 12)) {
      if (!t || typeof t !== 'object') continue
      const c = t as Partial<CustomTheme>
      if (typeof c.id === 'string' && /^[a-z0-9-]{1,32}$/.test(c.id) && typeof c.label === 'string' && c.label.trim() && typeof c.hue === 'number' && typeof c.tint === 'number' && typeof c.accent === 'string' && /^#[0-9a-fA-F]{6}$/.test(c.accent))
        custom.push({ id: c.id, label: c.label.trim().slice(0, 32), hue: Math.min(360, Math.max(0, Math.round(c.hue))), tint: Math.min(100, Math.max(0, Math.round(c.tint))), accent: c.accent.toLowerCase() })
    }
  const glass = typeof p.glass === 'number' && Number.isFinite(p.glass) ? Math.min(100, Math.max(50, Math.round(p.glass))) : DEFAULTS.glass
  const contrast = typeof p.contrast === 'number' && Number.isFinite(p.contrast) ? Math.min(150, Math.max(50, Math.round(p.contrast))) : DEFAULTS.contrast
  return {
    appearance: pick(p.appearance, ['system', 'light', 'dark'], DEFAULTS.appearance),
    theme: typeof p.theme === 'string' && /^[a-z0-9-]{1,32}$/.test(p.theme) ? p.theme : DEFAULTS.theme,
    contrast,
    density: pick(p.density, ['comfortable', 'compact'], DEFAULTS.density),
    contentWidth: pick(p.contentWidth, ['comfortable', 'wide', 'full'], DEFAULTS.contentWidth),
    statusColors: pick(p.statusColors, ['standard', 'colorblind'], DEFAULTS.statusColors),
    reduceMotion: bool(p.reduceMotion, DEFAULTS.reduceMotion),
    startPage: typeof p.startPage === 'string' && /^[a-z/-]{0,40}$/.test(p.startPage) ? p.startPage : DEFAULTS.startPage,
    timeMode: pick(p.timeMode, ['utc', 'local'], DEFAULTS.timeMode),
    confirmCancel: bool(p.confirmCancel, DEFAULTS.confirmCancel),
    notifications: pick(p.notifications, ['all', 'important', 'off'], DEFAULTS.notifications),
    liveUpdates: bool(p.liveUpdates, DEFAULTS.liveUpdates),
    sidebarWorkflows: bool(p.sidebarWorkflows, DEFAULTS.sidebarWorkflows),
    sidebarOpen: bool(p.sidebarOpen, DEFAULTS.sidebarOpen),
    agentDetails: bool(p.agentDetails, DEFAULTS.agentDetails),
    keybindings: kb,
    keyWhen: kw,
    glass,
    customThemes: custom,
  }
}

let current: Prefs = read()
const listeners = new Set<(p: Prefs) => void>()

export function getPrefs(): Prefs {
  return current
}

export function setPrefs(patch: Partial<Prefs>): Prefs {
  current = sanitize({ ...current, ...patch })
  try {
    if (PERSIST) window.localStorage.setItem(KEY, JSON.stringify(current))
  } catch {
    // The preference holds for this visit only.
  }
  for (const l of listeners) l(current)
  return current
}

export function resetPrefs(): Prefs {
  current = { ...DEFAULTS }
  try {
    if (PERSIST) window.localStorage.removeItem(KEY)
  } catch {
    // Nothing stored, nothing to remove.
  }
  for (const l of listeners) l(current)
  return current
}

/** The preferences, re-rendering the component when any of them changes. */
export function usePrefs(): Prefs {
  const [p, setP] = useState(current)
  useEffect(() => {
    listeners.add(setP)
    setP(current)
    return () => {
      listeners.delete(setP)
    }
  }, [])
  return p
}

export function subscribePrefs(f: (p: Prefs) => void): () => void {
  listeners.add(f)
  return () => {
    listeners.delete(f)
  }
}
