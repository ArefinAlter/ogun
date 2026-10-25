// The scope of the settings pages: "applying settings for <workflow> across <environment>", as in T3 Code's
// "All projects across all environments". It narrows what a settings page lists and which layer an edit writes
// to. It is held for the visit only and never stored.

import { useEffect, useState } from 'preact/hooks'

export interface Scope {
  /** A workflow name, or '' for every workflow. */
  workflow: string
  /** An environment id, or '' for every environment. */
  environment: string
}

let scope: Scope = { workflow: '', environment: '' }
const listeners = new Set<(s: Scope) => void>()

export function getScope(): Scope {
  return scope
}

export function setScope(patch: Partial<Scope>): void {
  scope = { ...scope, ...patch }
  for (const l of listeners) l(scope)
}

export function useScope(): Scope {
  const [s, setS] = useState(scope)
  useEffect(() => {
    listeners.add(setS)
    setS(scope)
    return () => {
      listeners.delete(setS)
    }
  }, [])
  return s
}
