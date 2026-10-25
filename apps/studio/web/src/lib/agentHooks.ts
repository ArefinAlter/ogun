// Hooks over the optional agent API (O-17): the thread list, one thread, and the settings the agent obeys.

import { useEffect, useState } from 'preact/hooks'

import type { AgentApi } from '../api/backend'
import type { AgentModel, AgentSettings, AgentThread, AgentThreadSummary } from '../api/types'
import { useAsync, useBackend } from './hooks'

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
