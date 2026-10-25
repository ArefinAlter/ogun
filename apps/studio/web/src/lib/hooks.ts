// Small hooks: async loading with reload, live refresh from the event stream, and the router.

import { createContext } from 'preact'
import { useCallback, useContext, useEffect, useRef, useState } from 'preact/hooks'

import type { Backend, ShellInfo } from '../api'
import { asOgunError } from '../api'
import type { EventView, OgunError } from '../api/types'
import { usePrefs } from './prefs'

export const BackendContext = createContext<Backend | null>(null)
export const InfoContext = createContext<{ info: ShellInfo | null; refresh: () => void }>({
  info: null,
  refresh: () => undefined,
})

export function useBackend(): Backend {
  const b = useContext(BackendContext)
  if (!b) throw new Error('no backend')
  return b
}

export function useInfo(): { info: ShellInfo | null; refresh: () => void } {
  return useContext(InfoContext)
}

export interface Async<T> {
  data: T | undefined
  error: OgunError | undefined
  loading: boolean
  reload: () => void
}

/** Runs `load` when `deps` change, keeping the last data while reloading. */
export function useAsync<T>(load: () => Promise<T>, deps: unknown[]): Async<T> {
  const [data, setData] = useState<T | undefined>(undefined)
  const [error, setError] = useState<OgunError | undefined>(undefined)
  const [loading, setLoading] = useState(true)
  const [tick, setTick] = useState(0)
  const gen = useRef(0)
  useEffect(() => {
    const mine = ++gen.current
    setLoading(true)
    load().then(
      (v) => {
        if (mine !== gen.current) return
        setData(v)
        setError(undefined)
        setLoading(false)
      },
      (e) => {
        if (mine !== gen.current) return
        setError(asOgunError(e))
        setLoading(false)
      },
    )
  }, [...deps, tick])
  const reload = useCallback(() => setTick((t) => t + 1), [])
  return { data, error, loading, reload }
}

/** Calls `onChange` (debounced) whenever an event arrives, for one run or for all. */
export function useLive(onChange: (e: EventView) => void, runId?: string, enabled = true): OgunError | undefined {
  const backend = useBackend()
  const [error, setError] = useState<OgunError | undefined>(undefined)
  const cb = useRef(onChange)
  cb.current = onChange
  const follow = usePrefs().liveUpdates
  useEffect(() => {
    if (!enabled || !follow) return undefined
    let timer: ReturnType<typeof setTimeout> | undefined
    let latest: EventView | undefined
    const sub = backend.subscribe(
      runId ? { runId } : {},
      (e) => {
        setError(undefined)
        latest = e
        if (timer === undefined) {
          timer = setTimeout(() => {
            timer = undefined
            if (latest) cb.current(latest)
          }, 300)
        }
      },
      (e) => setError(e),
    )
    return () => {
      sub.close()
      if (timer !== undefined) clearTimeout(timer)
    }
  }, [backend, runId, enabled, follow])
  return error
}

// ---------------------------------------------------------------- hash router

export interface Route {
  path: string[]
  query: URLSearchParams
}

function parseHash(): Route {
  const raw = window.location.hash.replace(/^#/, '') || '/'
  const [p, q] = raw.split('?', 2)
  return {
    path: (p ?? '/').split('/').filter(Boolean).map(decodeURIComponent),
    query: new URLSearchParams(q ?? ''),
  }
}

export function useRoute(): Route {
  const [route, setRoute] = useState(parseHash)
  useEffect(() => {
    const on = () => setRoute(parseHash())
    window.addEventListener('hashchange', on)
    return () => window.removeEventListener('hashchange', on)
  }, [])
  return route
}

/** Builds a hash link: href('runs', id) or href('runs', {workflow: 'x'}). */
export function href(...parts: (string | Record<string, string | number | undefined>)[]): string {
  const segs: string[] = []
  let q = ''
  for (const p of parts) {
    if (typeof p === 'string') segs.push(encodeURIComponent(p))
    else {
      const sp = new URLSearchParams()
      for (const [k, v] of Object.entries(p)) if (v !== undefined && v !== '') sp.set(k, String(v))
      const s = sp.toString()
      if (s) q = '?' + s
    }
  }
  return '#/' + segs.join('/') + q
}

export function navigate(to: string): void {
  window.location.hash = to.replace(/^#/, '')
}
