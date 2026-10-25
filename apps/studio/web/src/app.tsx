import { useCallback, useEffect, useMemo, useState } from 'preact/hooks'

import type { ShellInfo } from './api'
import { createBackend } from './api'
import { BackendContext, InfoContext, href, useAsync, useLive, useRoute } from './lib/hooks'
import type { IconName } from './ui/icons'
import { Icon, Mark } from './ui/icons'
import { About } from './views/About'
import { ApprovalDetail, Approvals } from './views/Approvals'
import { Artifacts } from './views/Artifacts'
import { Edges } from './views/Edges'
import { Gateway } from './views/Gateway'
import { Overview } from './views/Overview'
import { Partitions } from './views/Partitions'
import { RunDetail, Runs } from './views/Runs'
import { Settings } from './views/Settings'
import { StreamDetail, Streams } from './views/Streams'
import { WorkflowDetail, Workflows } from './views/Workflows'

type Theme = 'auto' | 'dark' | 'light'

interface NavItem {
  id: string
  label: string
  icon: IconName
  show?: (i: ShellInfo | null) => boolean
}

const NAV: NavItem[] = [
  { id: '', label: 'Overview', icon: 'gear' },
  { id: 'workflows', label: 'Workflows', icon: 'dag' },
  { id: 'runs', label: 'Runs', icon: 'hourglass' },
  { id: 'partitions', label: 'Partitions', icon: 'matrix' },
  { id: 'approvals', label: 'Approvals', icon: 'seal' },
  { id: 'artifacts', label: 'Artifacts', icon: 'cube' },
  { id: 'streams', label: 'Streams', icon: 'sieve' },
  { id: 'edges', label: 'Edges', icon: 'compass' },
  { id: 'gateway', label: 'Gateway', icon: 'anvil', show: (i) => !!i?.gateway },
  { id: 'settings', label: 'Settings', icon: 'drawers' },
  { id: 'about', label: 'About', icon: 'ledger' },
]

function ConnectionBadge({ info }: { info: ShellInfo | null }) {
  if (!info) return <span class="conn">connecting…</span>
  const text =
    info.link === 'local'
      ? `ogund${info.profile ? ` · ${info.profile}` : ''}`
      : info.link === 'hub'
        ? `hub · ${info.hubUrl ?? ''} · read-only`
        : info.link === 'server'
          ? info.user
            ? `${info.user.name} · ${info.user.roles.join(', ') || 'no role'}`
            : 'not signed in'
          : 'not connected'
  return (
    <span class={`conn ${info.connected ? 'ok' : 'bad'}`} title={info.error?.message ?? text}>
      <span class="dot" />
      {text}
    </span>
  )
}

function Page({ path, query }: { path: string[]; query: URLSearchParams }) {
  const [section, a, b] = path
  const q = (k: string) => query.get(k) ?? undefined
  switch (section ?? '') {
    case '':
      return <Overview />
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
      return <Settings />
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

function PendingCount({ backend }: { backend: ReturnType<typeof createBackend> }) {
  const status = useAsync(() => backend.status(), [backend])
  useLive(() => status.reload())
  const n = status.data?.approvals_pending ?? 0
  return n > 0 ? <span class="count" aria-label={`${n} pending`}>{n}</span> : null
}

let shared: ReturnType<typeof createBackend> | null = null
function useMemoBackend() {
  shared ??= createBackend()
  return shared
}

export function App() {
  const [backend, setBackend] = useState(useMemoBackend)
  const route = useRoute()
  const [info, setInfo] = useState<ShellInfo | null>(null)
  const [theme, setTheme] = useState<Theme>('auto')
  const refresh = useCallback(() => {
    let current = true
    backend.info().then((value) => { if (current) setInfo(value) }, () => { if (current) setInfo(null) })
    return () => { current = false }
  }, [backend])
  useEffect(refresh, [refresh])
  useEffect(() => {
    const root = document.documentElement
    if (theme === 'auto') delete root.dataset.theme
    else root.dataset.theme = theme
  }, [theme])
  useEffect(() => {
    if (info?.mobile) document.documentElement.classList.add('mobile')
  }, [info?.mobile])
  const ctx = useMemo(() => ({ info, refresh }), [info, refresh])
  const current = route.path[0] ?? ''
  const items = NAV.filter((n) => !n.show || n.show(info))
  const nextTheme: Record<Theme, Theme> = { auto: 'dark', dark: 'light', light: 'auto' }
  return (
    <BackendContext.Provider value={backend}>
      <InfoContext.Provider value={ctx}>
        <a class="skip" href="#main">
          Skip to content
        </a>
        <div class="shell">
          <aside class="side" aria-label="Sections">
            <a class="brand" href={href()}>
              <Mark />
              <span class="brand-name">
                OGUN
                <small>Studio</small>
              </span>
            </a>
            <nav class="nav">
              {items.map((n) => (
                <a href={href(...(n.id ? [n.id] : []))} aria-current={current === n.id ? 'page' : undefined}>
                  <Icon name={n.icon} />
                  {n.label}
                  {n.id === 'approvals' && info?.connected ? <PendingCount key={info.workspaceId ?? 'local'} backend={backend} /> : null}
                </a>
              ))}
            </nav>
            <div class="side-foot">Verne Ltd.</div>
          </aside>
          <header class="top">
            <ConnectionBadge info={info} />
            {info?.workspaces && info.workspaces.length > 1 ? (
              <label>
                Workspace{' '}
                <select aria-label="Workspace" value={info.workspaceId} onChange={(e) => {
                  if (!backend.selectWorkspace) return
                  const next = backend.selectWorkspace(e.currentTarget.value)
                  setInfo(null)
                  setBackend(next)
                }}>
                  {info.workspaces.map((w) => <option key={w.workspace_id} value={w.workspace_id}>{w.name} · {w.role}</option>)}
                </select>
              </label>
            ) : null}
            <span class="spacer" />
            <button
              type="button"
              class="btn small"
              onClick={() => setTheme(nextTheme[theme])}
              aria-label={`Theme: ${theme}. Change theme.`}
            >
              {theme === 'auto' ? 'Theme: system' : theme === 'dark' ? 'Theme: forge' : 'Theme: vellum'}
            </button>
          </header>
          <main class="main" id="main">
            {backend.kind === 'tauri' || info?.connected ? <Page key={info?.workspaceId ?? 'local'} path={route.path} query={route.query} /> : <div class="empty">{info?.error?.message ?? 'Connecting…'}</div>}
          </main>
        </div>
        <nav class="mobile-nav" aria-label="Sections">
          {items.map((n) => (
            <a href={href(...(n.id ? [n.id] : []))} aria-current={current === n.id ? 'page' : undefined}>
              <Icon name={n.icon} />
              {n.label}
            </a>
          ))}
        </nav>
      </InfoContext.Provider>
    </BackendContext.Provider>
  )
}
