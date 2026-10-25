// The agent screen: a conversation where a person gives Ogun's agent a data-engineering task (O-17, proposed). The
// first message starts a thread; the thread is the page. The agent reads, explains, validates and plans through a
// closed set of tools, proposes changes as cards, and leaves every consequential press to the person.

import { useEffect, useMemo, useState } from 'preact/hooks'

import type { AgentAccess, Mention } from '../api/types'
import { asOgunError } from '../api'
import { ACCESS_INFO } from '../lib/agentTools'
import { useAgent, useAgentModels, useAgentSettings, useAgentThread } from '../lib/agentHooks'
import { href, navigate, useBackend, useInfo } from '../lib/hooks'
import { usePrefs } from '../lib/prefs'
import { setPrefs } from '../lib/prefs'
import { CodeCopy, Notice } from '../ui/components'
import { Icon, Mark } from '../ui/icons'
import type { IconName } from '../ui/icons'
import { Composer } from './agent/Composer'
import { Details } from './agent/Details'
import { Timeline } from './agent/Timeline'

const STARTERS: { icon: IconName; text: string; hint: string }[] = [
  { icon: 'gauge', text: 'What is running, and what is waiting for a person?', hint: 'A summary of the daemon, read live' },
  { icon: 'matrix', text: 'Backfill trips-backfill since 2019', hint: 'Plans the range; starting it is your press' },
  { icon: 'refresh', text: 'Why did the ridership forecast fail last night?', hint: 'Reads the run and explains the cause' },
  { icon: 'dag', text: 'Draft a daily workflow that lists trips per station from the open-trips archive', hint: 'Validated before you see it' },
  { icon: 'cube', text: 'Which stations are likely to run empty or full today?', hint: 'From the latest rebalance-plan shortlist' },
  { icon: 'plug', text: 'Which connectors would I need to send alerts to Datadog and DingTalk?', hint: 'From the connector catalogue' },
]

/** A daemon with no agent threads: say so, and show the way that works today. */
function Unavailable() {
  const { info } = useInfo()
  const origin = typeof window !== 'undefined' && info?.link === 'server' ? window.location.origin : 'https://your-hub.example'
  return (
    <div class="agent-empty">
      <div class="hero">
        <Mark />
        <h1>Agent threads are not part of this daemon yet</h1>
        <p class="muted">
          The conversation screen needs a daemon that runs agent sessions (O-17, proposed). This one does not, so Studio does not pretend: nothing here can talk to a model.
        </p>
      </div>
      <Notice>
        <strong>An agent can already work on this daemon.</strong> Any coding agent that speaks MCP gets the same closed tool set, with the same limits: it reads, validates and plans, can start runs with an idempotency key, and can never decide an approval.
      </Notice>
      <h2>On this machine, over stdio (read only)</h2>
      <CodeCopy text="claude mcp add ogun -- ogun mcp" />
      <h2>Against a hub, over HTTP (an API token acts as an agent)</h2>
      <CodeCopy text={`claude mcp add --transport http ogun ${origin}/mcp --header "Authorization: Bearer ogt1_..."`} />
      <p class="muted small-text">
        Create the token in Studio-web, signed in. <a href={href('settings', 'environments')}>Environments</a> shows how this Studio is linked.
      </p>
    </div>
  )
}

function NewThread() {
  const agent = useAgent()
  const models = useAgentModels()
  const settings = useAgentSettings()
  const [access, setAccess] = useState<AgentAccess | null>(null)
  const [model, setModel] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const effectiveAccess = access ?? settings.data?.default_access ?? 'read'
  const effectiveModel = model ?? settings.data?.default_model ?? 'scripted'

  const start = async (text: string, mentions: Mention[]) => {
    if (!agent) return
    setError(null)
    try {
      const t = await agent.create({ access: effectiveAccess, model: effectiveModel })
      navigate(href('agent', t.thread_id))
      await agent.send(t.thread_id, text, mentions)
    } catch (e) {
      setError(asOgunError(e).message)
    }
  }
  return (
    <div class="agent-new">
      <div class="agent-scroll">
        <div class="hero">
          <Mark />
          <h1>What should Ogun work on?</h1>
          <p class="muted">Describe a data-engineering task. The agent reads this daemon, drafts and validates, and proposes. You press the buttons that change anything.</p>
        </div>
        <div class="starters" role="list" aria-label="Start from">
          <div class="label">Start from</div>
          {STARTERS.map((s) => (
            <button
              type="button"
              role="listitem"
              onClick={() => {
                const wf = /@?(trips-backfill)/.exec(s.text)
                void start(s.text, wf ? [{ kind: 'workflow', id: 'trips-backfill', label: 'trips-backfill' }] : [])
              }}
            >
              <Icon name={s.icon} />
              <span class="name">{s.text}</span>
              <span class="meta">{s.hint}</span>
            </button>
          ))}
        </div>
        {error ? <Notice kind="error">{error}</Notice> : null}
      </div>
      <Composer
        draftKey="new"
        busy={false}
        access={effectiveAccess}
        onAccess={setAccess}
        model={effectiveModel}
        onModel={setModel}
        models={models ?? []}
        onSend={(t, m) => void start(t, m)}
        onStop={() => undefined}
      />
    </div>
  )
}

function ThreadPage({ threadId }: { threadId: string }) {
  const agent = useAgent()
  const thread = useAgentThread(threadId)
  const models = useAgentModels()
  const prefs = usePrefs()
  const [error, setError] = useState<string | null>(null)
  useEffect(() => setError(null), [threadId])
  const sendFailure = useMemo(() => (e: unknown) => setError(asOgunError(e).message), [])

  if (thread === undefined) return <div class="loading">loading…</div>
  if (thread === null)
    return (
      <div class="empty">
        This thread no longer exists. <a href={href('agent')}>Start a new one</a>
      </div>
    )
  return (
    <div class={`agent-thread ${prefs.agentDetails ? 'with-side' : ''}`}>
      <div class="agent-col">
        <Timeline thread={thread} />
        {error ? (
          <div class="agent-err">
            <Notice kind="error">{error}</Notice>
          </div>
        ) : null}
        <Composer
          draftKey={thread.thread_id}
          busy={thread.state === 'running'}
          access={thread.access}
          onAccess={(a) => void agent?.setAccess(thread.thread_id, a)}
          model={thread.model}
          onModel={() => undefined}
          lockModel
          models={models ?? []}
          placeholder={`Reply. This thread has ${ACCESS_INFO[thread.access].label} access.`}
          onSend={(t, m) => {
            setError(null)
            agent?.send(thread.thread_id, t, m).catch(sendFailure)
          }}
          onStop={() => void agent?.stop(thread.thread_id)}
        />
      </div>
      {prefs.agentDetails ? <Details thread={thread} /> : null}
    </div>
  )
}

export function Agent({ threadId }: { threadId?: string }) {
  const agent = useAgent()
  useBackend()
  if (!agent) return <Unavailable />
  return threadId ? <ThreadPage threadId={threadId} /> : <NewThread />
}

/** The button in the top bar that shows or hides the side panel of a thread. */
export function DetailsToggle() {
  const prefs = usePrefs()
  return (
    <button type="button" class="icon-btn" aria-pressed={prefs.agentDetails} aria-label="Thread details" title="Thread details" onClick={() => setPrefs({ agentDetails: !prefs.agentDetails })}>
      <Icon name="panelright" />
    </button>
  )
}
