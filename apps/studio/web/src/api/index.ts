// Picks the backend: inside a Tauri shell the closed command set, otherwise Studio-web's REST routes.

import type { Backend } from './backend'
import { httpBackend } from './http'
import { tauriBackend } from './tauri'

export function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

export function createBackend(): Backend {
  return inTauri() ? tauriBackend() : httpBackend()
}

export type { Backend, PreparedDecision, ShellInfo } from './backend'
export { asOgunError } from './backend'
