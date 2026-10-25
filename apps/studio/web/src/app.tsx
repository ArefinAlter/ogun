import { useCallback, useEffect, useMemo, useState } from 'preact/hooks'

import type { Backend, ShellInfo } from './api'
import type { EventView, WorkflowSummary } from './api/types'
import { useCatalog } from './connectors'
import { useAgent, useAgentThreads } from './lib/agentHooks'
import { fmtAgo } from './lib/format'
import { BackendContext, InfoContext, href, navigate, useAsync, useBackend, useLive, useRoute } from './lib/hooks'
import { commandFor, formatCombo, bindingOf } from './lib/keys'
import type { NavItem } from './lib/nav'
import { NAV } from './lib/nav'
import { getPrefs, setPrefs, usePrefs } from './lib/prefs'
import { useWindowChrome } from './lib/shell'
import { applyPrefs, watchSystemScheme } from './lib/themes'
import { Palette } from './ui/Palette'
import type { IconName } from './ui/icons'
import { Icon, Mark } from './ui/icons'
import { WindowControls } from './ui/WindowControls'
import { About } from './views/About'
import { ApprovalDetail, Approvals } from './views/Approvals'
import { Artifacts } from './views/Artifacts'
import { Edges } from './views/Edges'
import { Gateway } from './views/Gateway'
import { Agent, DetailsToggle } from './views/Agent'
import { Overview } from './views/Overview'
import { Partitions } from './views/Partitions'
import { RunDetail, Runs } from './views/Runs'
import { SETTINGS_SECTIONS, Settings } from './views/settings'
import { StreamDetail, Streams } from './views/Streams'
import { WorkflowDetail, Workflows } from './views/Workflows'

const TITLES: Record<string, string> = {
  '': 'Overview',
  agent: 'Agent',
  workflows: 'Workflows',
  runs: 'Runs',
  partitions: 'Partitions',
  approvals: 'Approvals',
  artifacts: 'Artifacts',
  streams: 'Streams',
  edges: 'Edges',
  gateway: 'Field gateway',
  settings: 'Settings',
  about: 'About',
}

function connectionText(info: ShellInfo | null): string {
  if (!info) return 'connecting…'
  if (info.link === 'local') return `ogund${info.profile ? ` · ${info.profile}` : ''}`
  if (info.link === 'hub') return `hub · ${info.hubUrl ?? ''} · read-only`
  if (info.link === 'server') return info.user ? `${info.user.name} · ${info.user.roles.join(', ') || 'no role'}` : 'not signed in'
  return 'not connected'
}

function ConnectionBadge({ info }: { info: ShellInfo | null }) {
  const text = connectionText(info)
  return (
    <span class={`conn ${info ? (info.connected ? 'ok' : 'bad') : ''}`} title={info?.error?.message ?? text}>
      <span class="dot" />
      <span class="name">{text}</span>
    </span>
  )
}

function Page({ path, query }: { path: string[]; query: URLSearchParams }) {
  const [section, a, b] = path
  const q = (k: string) => query.get(k) ?? undefined
  switch (section ?? '') {
    case '':
      return <Overview />
    case 'agent':
      return <Agent {...(a ? { threadId: a } : {})} />
    case 'workflows': {
      if (!a) return <Workflows />
      const v = q('version')
      return <WorkflowDetail name={a} {...(v && /^\d+$/.test(v) ? { version: Number(v) } : {})} />
    }
    case 'runs':
      return a ? <RunDetail runId={a} /> : <Runs {...(q('workflow') ? { workflow: q('workflow') as string } : {})} />
    case 'partitions':
      return <Partitions {...(q('workflow') ? { workflow: q('workflow') as string } : {})} />
    case 'approvals':
      return a ? <ApprovalDetail approvalId={a} /> : <Approvals {...(q('state') ? { state: q('state') as string } : {})} />
    case 'artifacts':
      return <Artifacts {...(q('run') ? { runId: q('run') as string } : {})} {...(q('blake3') ? { blake3: q('blake3') as string } : {})} />
    case 'streams':
      return a && b ? <StreamDetail workflow={a} node={b} /> : <Streams />
    case 'edges':
      return <Edges />
    case 'gateway':
      return <Gateway />
    case 'settings':
      return <Settings path={path} query={query} />
    case 'about':
      return <About />
    default:
      return (
        <div class="empty">
          No such page. <a href={href()}>Overview</a>
        </div>
      )
  }
}

function PendingCount() {
  const backend = useBackend()
  const status = useAsync(() => backend.status(), [backend])
  useLive(() => status.reload())
  const n = status.data?.approvals_pending ?? 0
  return n > 0 ? <span class="count" aria-label={`${n} pending`}>{n}</span> : null
}

function workflowDot(w: WorkflowSummary): string {
  if (!w.runnable) return 'warn'
  return w.enabled ? 'ok' : ''
}

/** The stored workflows, as rows under the sections (T3 Code lists threads the same way). */
function WorkflowRows({ connected, currentName }: { connected: boolean; currentName?: string }) {
  const backend = useBackend()
  const list = useAsync(() => (connected ? backend.workflowsList() : Promise.resolve(null)), [backend, connected])
  useLive(() => list.reload(), undefined, connected)
  const rows = list.data?.workflows ?? []
  if (!rows.length) return null
  return (
    <div class="nav-group" aria-label="Stored workflows">
      <div class="nav-head">Stored workflows</div>
      {rows.map((w) => (
        <a href={href('workflows', w.name)} aria-current={currentName === w.name ? 'page' : undefined} title={w.untrusted_description ?? w.name}>
          <span class={`dot ${workflowDot(w)}`} aria-hidden="true" />
          <span class="name">{w.name}</span>
          <span class="trail">
            v{w.latest_version} · {fmtAgo(w.updated_at).replace(' ago', '')}
          </span>
        </a>
      ))}
    </div>
  )
}

/** Recent agent threads under the sections, as T3 Code lists its threads. */
function ThreadRows({ currentId }: { currentId?: string }) {
  const threads = useAgentThreads()
  const agent = useAgent()
  const [all, setAll] = useState(false)
  if (!threads?.length) return null
  const shown = all ? threads : threads.slice(0, 7)
  return (
    <div class="nav-group" aria-label="Agent threads">
      <div class="nav-head">Threads</div>
      {shown.map((t) => (
        <a class="threadrow" href={href('agent', t.thread_id)} aria-current={currentId === t.thread_id ? 'page' : undefined} title={t.title}>
          <span class={`dot ${t.state === 'running' ? 'run' : t.state === 'waiting' ? 'warn' : ''}`} aria-hidden="true" />
          <span class="name">{t.title}</span>
          <span class="trail">{fmtAgo(t.updated_at).replace(' ago', '')}</span>
          <button
            type="button"
            class="rowx"
            aria-label={`Delete thread ${t.title}`}
            title="Delete this thread"
            onClick={(e) => {
              e.preventDefault()
              e.stopPropagation()
              void agent?.remove(t.thread_id)
              if (currentId === t.thread_id) navigate(href('agent'))
            }}
          >
            <Icon name="trash" />
          </button>
        </a>
      ))}
      {threads.length > 7 ? (
        <button type="button" class="showmore" onClick={() => setAll((v) => !v)}>
          <Icon name={all ? 'up' : 'plus'} /> {all ? 'Show fewer' : `Show ${threads.length - 7} more`}
        </button>
      ) : null}
    </div>
  )
}

/** The title of the open thread, for the breadcrumb. */
function ThreadTitle({ id }: { id: string }) {
  const threads = useAgentThreads()
  return <span class="here">{threads?.find((t) => t.thread_id === id)?.title ?? 'Thread'}</span>
}

interface Toast {
  id: number
  kind: 'ok' | 'bad' | 'warn' | ''
  title: string
  desc?: string
  /** Shown at the "important" level too. */
  important: boolean
}

function describeEvent(e: EventView): Toast | null {
  const k = e.kind as Record<string, unknown> | null
  if (!k || typeof k !== 'object') return null
  const t = String(k.type ?? '')
  switch (t) {
    case 'daemon.stopped':
      return { id: e.seq, kind: 'bad', important: true, title: 'ogund stopped (SIGKILL)', desc: `${String(k.runs_active)} runs active, ${String(k.attempts_in_flight)} attempts in flight` }
    case 'daemon.started':
      return { id: e.seq, kind: 'ok', important: true, title: 'ogund started; runs resumed', desc: `${String(k.re_run)} in-flight attempts run again; finished partitions untouched` }
    case 'window.revised':
      return { id: e.seq, kind: 'warn', important: true, title: `Window ${String(k.partition)} revised`, desc: `${String(k.late)} late records became revision ${String(k.revision)}, not a gap` }
    case 'approval':
      return { id: e.seq, kind: k.state === 'approved' ? 'ok' : 'warn', important: true, title: `${String(k.kind).replace('_', ' ')} ${String(k.state)}`, desc: `${String(k.workflow)}${k.node ? ' · ' + String(k.node) : ''}` }
    case 'transition':
      if (k.to === 'failed') return { id: e.seq, kind: 'bad', important: true, title: 'A node instance failed', desc: `${String(k.node ?? e.instance_id ?? '')}${k.cause ? ' · ' + String(k.cause) : ''}` }
      return null
    case 'connection.saved':
      return { id: e.seq, kind: 'ok', important: false, title: 'Connection saved', desc: `${String(k.name)} · ${String(k.connector)}` }
    case 'settings.runtime':
      return { id: e.seq, kind: '', important: false, title: 'Run defaults changed', desc: `${String(k.workflow)} · ${Array.isArray(k.fields) ? k.fields.join(', ') : ''}` }
    case 'storage.cleanup':
      return { id: e.seq, kind: 'ok', important: false, title: 'Cleanup finished', desc: `${String(k.artifacts)} artifacts removed` }
    case 'run':
      // A stream seals a window a minute; its one-partition successes would be noise.
      if (k.state === 'succeeded' && Number(k.partitions ?? 1) <= 1) return null
      if (k.state === 'succeeded' || k.state === 'failed' || k.state === 'cancelled')
        return { id: e.seq, kind: k.state === 'succeeded' ? 'ok' : k.state === 'failed' ? 'bad' : '', important: k.state === 'failed', title: `Run ${String(k.state)}`, desc: `${String(k.workflow ?? '')} · ${e.run_id}` }
      return null
    default:
      return null
  }
}

function Toasts({ toasts, dismiss }: { toasts: Toast[]; dismiss: (id: number) => void }) {
  if (!toasts.length) return null
  return (
    <div class="toasts" aria-live="polite">
      {toasts.map((t) => (
        <div class={`toast ${t.kind}`} key={t.id}>
          <span class="dot" />
          <div class="text">
            <div class="title">{t.title}</div>
            {t.desc ? <div class="desc">{t.desc}</div> : null}
          </div>
          <button type="button" class="x" aria-label="Dismiss" onClick={() => dismiss(t.id)}>
            ×
          </button>
        </div>
      ))}
    </div>
  )
}

let lastOutside = '#/'

export function App({ backend }: { backend: Backend }) {
  const route = useRoute()
  const prefs = usePrefs()
  const [info, setInfo] = useState<ShellInfo | null>(null)
  const [palette, setPalette] = useState<null | 'all' | 'themes'>(null)
  const [toasts, setToasts] = useState<Toast[]>([])
  const catalog = useCatalog()
  const chrome = useWindowChrome()
  // The top bars are the window's title bar when the page draws its own controls (Tauri on Windows).
  // `deep`: a press anywhere in the bar moves the window, except on a button or a link.
  const drag = chrome === 'custom' ? { 'data-tauri-drag-region': 'deep' } : {}
  const refresh = useCallback(() => {
    backend.info().then(setInfo, () => setInfo(null))
  }, [backend])
  useEffect(refresh, [refresh])

  // Preferences become the look of the document, and follow the system scheme when asked to.
  useEffect(() => {
    applyPrefs(prefs)
  }, [prefs])
  useEffect(() => watchSystemScheme(() => applyPrefs(getPrefs())), [])
  useEffect(() => {
    if (info?.mobile) document.documentElement.classList.add('mobile')
  }, [info?.mobile])

  // The start page applies when the address names no page.
  useEffect(() => {
    if (!window.location.hash || window.location.hash === '#' || window.location.hash === '#/') {
      // The agent is the first page where the daemon offers one; a daemon without it opens on Overview.
      const pref = getPrefs().startPage
      const start = pref === 'agent' && !backend.agent ? '' : pref
      if (start) navigate(href(start))
    }
  }, [backend])

  const section = route.path[0] ?? ''
  useEffect(() => {
    if (section !== 'settings') lastOutside = window.location.hash || '#/'
  }, [section, route])

  // Shortcuts.
  useEffect(() => {
    const NAV_TARGETS: Record<string, string[]> = {
      'nav.agent': ['agent'],
      'nav.overview': [],
      'nav.workflows': ['workflows'],
      'nav.runs': ['runs'],
      'nav.partitions': ['partitions'],
      'nav.approvals': ['approvals'],
      'nav.artifacts': ['artifacts'],
      'nav.streams': ['streams'],
      'nav.edges': ['edges'],
      'nav.connectors': ['settings', 'connectors'],
    }
    const onKey = (e: KeyboardEvent) => {
      const c = commandFor(e, getPrefs())
      if (!c) return
      e.preventDefault()
      switch (c.id) {
        case 'palette.toggle':
          setPalette((v) => (v ? null : 'all'))
          break
        case 'settings.open':
          navigate(href('settings', 'general'))
          break
        case 'agent.new':
          if (backend.agent) navigate(href('agent'))
          break
        case 'sidebar.toggle':
          setPrefs({ sidebarOpen: !getPrefs().sidebarOpen })
          break
        case 'theme.pick':
          setPalette('themes')
          break
        case 'appearance.cycle': {
          const order = ['system', 'light', 'dark'] as const
          const next = order[(order.indexOf(getPrefs().appearance) + 1) % order.length] as (typeof order)[number]
          setPrefs({ appearance: next })
          const t: Toast = { id: Date.now(), kind: '', important: true, title: `Appearance: ${next === 'system' ? 'System' : next === 'light' ? 'Light' : 'Dark'}` }
          setToasts((ts) => [...ts.slice(-3), t])
          setTimeout(() => setToasts((ts) => ts.filter((x) => x.id !== t.id)), 2500)
          break
        }
        default: {
          const to = NAV_TARGETS[c.id]
          if (to) navigate(href(...to))
        }
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [backend])

  // Events become toasts according to the notification preference; a daemon restart refreshes the badge.
  const dismiss = useCallback((id: number) => setToasts((ts) => ts.filter((t) => t.id !== id)), [])
  useEffect(() => {
    const sub = backend.subscribe({}, (e) => {
      const k = e.kind as Record<string, unknown> | null
      if (k && typeof k === 'object' && String(k.type ?? '').startsWith('daemon.')) refresh()
      const level = getPrefs().notifications
      if (level === 'off') return
      const t = describeEvent(e)
      if (!t || (level === 'important' && !t.important)) return
      setToasts((ts) => [...ts.slice(-3), t])
      setTimeout(() => dismiss(t.id), 7000)
    })
    return () => sub.close()
  }, [backend, refresh, dismiss])

  const ctx = useMemo(() => ({ info, refresh }), [info, refresh])
  const inSettings = section === 'settings'
  const items = NAV.filter((n) => !n.show || n.show(info))
  const groups = [...new Set(items.map((n) => n.group))]
  const detail = route.path[1]
  const settingsId = route.path[1] ?? 'general'
  const settingsSection = SETTINGS_SECTIONS.find((s) => s.id === settingsId)
  const nextSchemeLabel = prefs.appearance === 'system' ? 'System' : prefs.appearance === 'dark' ? 'Dark' : 'Light'
  const title = TITLES[section] ?? section
  const connectorName = inSettings && settingsId === 'connectors' && route.path[2] ? catalog?.find((m) => m.id === route.path[2])?.name : undefined

  const crumbs = inSettings ? (
    <>
      <a href={href('settings', 'general')}>Settings</a>
      <span class="sep">/</span>
      {route.path[2] ? (
        <>
          <a href={href('settings', settingsId)}>{settingsSection?.label ?? settingsId}</a>
          <span class="sep">/</span>
          <span class="here">{connectorName ?? route.path[2]}</span>
        </>
      ) : (
        <span class="here">{settingsSection?.label ?? settingsId}</span>
      )}
    </>
  ) : (
    <>
      <a href={href()}>{info?.profile ?? (info?.link === 'server' ? 'server' : 'ogund')}</a>
      <span class="sep">/</span>
      {detail && section === 'agent' ? (
        <>
          <a href={href(section)}>{title}</a>
          <span class="sep">/</span>
          <ThreadTitle id={detail} />
        </>
      ) : detail && section !== 'streams' ? (
        <>
          <a href={href(section)}>{title}</a>
          <span class="sep">/</span>
          <span class="here mono">{detail}</span>
        </>
      ) : (
        <span class="here">{title}</span>
      )}
    </>
  )

  return (
    <BackendContext.Provider value={backend}>
      <InfoContext.Provider value={ctx}>
        <a class="skip" href="#main">
          Skip to content
        </a>
        <div class={`shell ${prefs.sidebarOpen ? '' : 'side-closed'}${chrome ? ` chrome-${chrome}` : ''}${section === 'agent' ? ' is-chat' : ''}`}>
          <aside class="side" aria-label={inSettings ? 'Settings sections' : 'Sections'}>
            <div class="brand" {...drag}>
              <button type="button" class="icon-btn brand-toggle" onClick={() => setPrefs({ sidebarOpen: false })} aria-label="Hide the sidebar" title={`Hide the sidebar (${formatCombo(bindingOf(prefs, 'sidebar.toggle')).join('+')})`}>
                <Icon name="panel" />
              </button>
              <a class="brand-link" href={href()}>
                <Mark />
                <span class="brand-name">
                  Ogun<small>Studio</small>
                </span>
              </a>
              <span class="spacer" />
            </div>
            {backend.agent && !inSettings ? (
              <a class="newthread" href={href('agent')} title={`New thread (${formatCombo(bindingOf(prefs, 'agent.new')).join('+')})`}>
                <Icon name="compose" />
                New thread
                <span class="spacer" />
                {formatCombo(bindingOf(prefs, 'agent.new')).map((k) => (
                  <kbd>{k}</kbd>
                ))}
              </a>
            ) : null}
            <button type="button" class="side-search" onClick={() => setPalette('all')}>
              <Icon name="search" />
              Search
              <span class="spacer" />
              {formatCombo(bindingOf(prefs, 'palette.toggle')).map((k) => (
                <kbd>{k}</kbd>
              ))}
            </button>
            {inSettings ? (
              <nav class="nav settings-nav">
                {SETTINGS_SECTIONS.map((s) => (
                  <a href={href('settings', s.id)} aria-current={settingsId === s.id ? 'page' : undefined}>
                    <Icon name={s.icon} />
                    <span class="name">{s.label}</span>
                  </a>
                ))}
              </nav>
            ) : (
              <nav class="nav">
                {groups.map((g) => (
                  <div class="nav-group" key={g}>
                    <div class="nav-head">{g}</div>
                    {items
                      .filter((n) => n.group === g)
                      .map((n) => (
                        <a href={href(...(n.to ?? (n.id ? [n.id] : [])))} aria-current={(n.to ? false : section === n.id) ? 'page' : undefined}>
                          <Icon name={n.icon} />
                          <span class="name">{n.label}</span>
                          {n.id === 'approvals' && info?.connected ? <PendingCount /> : null}
                        </a>
                      ))}
                  </div>
                ))}
                {backend.agent ? <ThreadRows {...(section === 'agent' && detail ? { currentId: detail } : {})} /> : null}
                {prefs.sidebarWorkflows ? <WorkflowRows connected={!!info?.connected} {...(section === 'workflows' && detail ? { currentName: detail } : {})} /> : null}
              </nav>
            )}
            <div class="side-foot">
              {inSettings ? (
                <a class="backlink" href={lastOutside}>
                  <Icon name="arrowleft" /> Back
                </a>
              ) : (
                <>
                  <a class="icon-btn" href={href('settings', 'general')} aria-label="Settings" title={`Settings (${formatCombo(bindingOf(prefs, 'settings.open')).join('+')})`}>
                    <Icon name="gear" />
                  </a>
                  <a class="icon-btn" href={href('about')} aria-label="About" title="About Ogun Studio" aria-current={section === 'about' ? 'page' : undefined}>
                    <Icon name="info" />
                  </a>
                  <button type="button" class="icon-btn" onClick={() => setPalette('themes')} aria-label={`Appearance: ${nextSchemeLabel}. Change theme.`} title={`Appearance: ${nextSchemeLabel}`}>
                    <Icon name={prefs.appearance === 'dark' ? 'moon' : prefs.appearance === 'light' ? 'sun' : 'contrast'} />
                  </button>
                </>
              )}
              <span class="spacer" />
              <ConnectionBadge info={info} />
            </div>
          </aside>
          <header class="top" {...drag}>
            <button type="button" class="icon-btn top-toggle" onClick={() => setPrefs({ sidebarOpen: true })} aria-label="Show the sidebar" title={`Show the sidebar (${formatCombo(bindingOf(prefs, 'sidebar.toggle')).join('+')})`}>
              <Icon name="panel" />
            </button>
            <nav class="crumbs" aria-label="Breadcrumb">
              {crumbs}
            </nav>
            <span class="spacer" />
            {section === 'agent' && detail ? <DetailsToggle /> : null}
            {chrome === 'custom' ? <WindowControls /> : null}
          </header>
          <main class={`main ${section === 'agent' ? 'chat' : ''}`} id="main">
            <Page path={route.path} query={route.query} />
          </main>
        </div>
        <nav class="mobile-nav" aria-label="Sections">
          {[...items, { id: 'settings', label: 'Settings', icon: 'gear' as IconName, group: '' }].map((n) => (
            <a href={href(...((n as NavItem).to ?? (n.id ? [n.id] : [])))} aria-current={section === n.id ? 'page' : undefined}>
              <Icon name={n.icon} />
              {n.label}
            </a>
          ))}
        </nav>
        {palette ? <Palette backend={backend} mode={palette} onClose={() => setPalette(null)} go={(to) => navigate(to)} /> : null}
        <Toasts toasts={toasts} dismiss={dismiss} />
      </InfoContext.Provider>
    </BackendContext.Provider>
  )
}

