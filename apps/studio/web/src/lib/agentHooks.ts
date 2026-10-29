// Hooks over the optional agent API (O-17): the thread list, one thread, and the settings the agent obeys.

import { useEffect, useState } from 'preact/hooks'

import type { AgentApi } from '../api/backend'
import type { AgentModel, AgentSettings, AgentThread, AgentThreadSummary } from '../api/types'
import { agentReadiness } from './agentGate'
import type { Readiness } from './agentGate'
import { useAsync, useBackend, useInfo } from './hooks'
import { readLadder } from './ladder'

/** The agent API of the backend, or undefined when the daemon offers none. */
export function useAgent(): AgentApi | undefined {
  return useBackend().agent
}

export function useAgentThreads(): AgentThreadSummary[] | undefined {
  const agent = useAgent()
  const [threads, setThreads] = useState<AgentThreadSummary[] | undefined>(undefined)
  useEffect(() => {
    if (!agent) return undefined
    const sub = agent.watchThreads(setThreads)
    return () => sub.close()
  }, [agent])
  return threads
}

/** One thread, kept current while it streams. `null` once it is gone; `undefined` while loading. */
export function useAgentThread(threadId: string | undefined): AgentThread | null | undefined {
  const agent = useAgent()
  const [thread, setThread] = useState<AgentThread | null | undefined>(undefined)
  useEffect(() => {
    setThread(undefined)
    if (!agent || !threadId) return undefined
    const sub = agent.watchThread(threadId, setThread)
    return () => sub.close()
  }, [agent, threadId])
  return thread
}

export function useAgentModels(): AgentModel[] | undefined {
  const agent = useAgent()
  return useAsync(() => (agent ? agent.models() : Promise.resolve([])), [agent]).data
}

export function useAgentSettings() {
  const agent = useAgent()
  return useAsync<AgentSettings | null>(() => (agent ? agent.settings() : Promise.resolve(null)), [agent])
}

/**
 * Whether the real agent of the desktop's own engine can answer now, and if not why. `null` where it does not
 * apply (the scripted and hosted agents call no model) or while the facts are still loading.
 */
export function useAgentReadiness(): { readiness: Readiness | null; reload: () => void } {
  const backend = useBackend()
  const { info } = useInfo()
  const applies = !!backend.agent && !!backend.engine && info?.link === 'local'
  const settings = useAgentSettings()
  const models = useAgentModels()
  const ladder = useAsync(() => (applies ? backend.llmLadder().catch(() => null) : Promise.resolve(null)), [backend, applies])
  const secrets = useAsync(() => (applies ? backend.secretsList().catch(() => null) : Promise.resolve(null)), [backend, applies])
  const reload = () => {
    settings.reload()
    ladder.reload()
    secrets.reload()
  }
  if (!applies || settings.loading || ladder.loading || secrets.loading) return { readiness: null, reload }
  const providers = ladder.data?.available ? readLadder(ladder.data.ladder) : null
  const availableModels = (models ?? []).filter((m) => m.available).map((m) => m.id)
  return { readiness: agentReadiness({ settings: settings.data, providers, secretNames: secrets.data?.names ?? [], availableModels }), reload }
}
