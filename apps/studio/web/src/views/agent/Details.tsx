// The side panel of a thread: what the agent may do here, which tools exist and which are open to it at this access,
// what you pointed it at, and the rules no setting changes.

import { useState } from 'preact/hooks'

import type { AgentAccess, AgentThread } from '../../api/types'
import { ACCESS_INFO, ACCESS_ORDER, NEVER, TOOLS, accessRank, mayCall } from '../../lib/agentTools'
import { useAgent, useAgentSettings } from '../../lib/agentHooks'
import { fmtAgo } from '../../lib/format'
import { href, navigate } from '../../lib/hooks'
import { setPrefs } from '../../lib/prefs'
import { Chip } from '../../ui/components'
import { Icon } from '../../ui/icons'

function Section({ title, children }: { title: string; children: preact.ComponentChildren }) {
  return (
    <section class="dsec">
      <h3>{title}</h3>
      {children}
    </section>
  )
}

export function Details({ thread }: { thread: AgentThread }) {
  const agent = useAgent()
  const settings = useAgentSettings()
  const [renaming, setRenaming] = useState(false)
  const [title, setTitle] = useState(thread.title)
  const enabled = settings.data?.tools_enabled ?? TOOLS.map((t) => t.name)
  const calls = thread.items.filter((i) => i.kind === 'tool')
  const refused = calls.filter((i) => i.kind === 'tool' && i.state === 'denied').length
  return (
    <aside class="agent-side" aria-label="Thread details">
      <div class="details-heading"><strong>Thread details</strong><button type="button" class="icon-btn" aria-label="Close thread details" onClick={() => setPrefs({ agentDetails: false })}><Icon name="x" /></button></div>
      <Section title="Thread">
        <dl class="kvs">
          <dt>state</dt>
          <dd>
            <Chip tone={thread.state === 'running' ? 'accent' : thread.state === 'waiting' ? 'warn' : ''}>{thread.state === 'waiting' ? 'waiting for you' : thread.state}</Chip>
          </dd>
          <dt>model</dt>
          <dd>{thread.model === 'scripted' ? 'Ogun agent, prepared answers' : thread.model}</dd>
          {thread.spend_usd_micro !== undefined ? (
            <>
              <dt>spend</dt>
              <dd>
                ${(thread.spend_usd_micro / 1_000_000).toFixed(4)}
                {settings.data ? ` of $${settings.data.spend_cap_usd.toFixed(2)}` : ''}
                {thread.input_tokens !== undefined ? <span class="muted"> · {thread.input_tokens.toLocaleString('en-US')} in, {(thread.output_tokens ?? 0).toLocaleString('en-US')} out</span> : null}
              </dd>
            </>
          ) : null}
          <dt>started</dt>
          <dd>{fmtAgo(thread.created_at)}</dd>
          <dt>tool calls</dt>
          <dd>
            {calls.length}
            {refused ? `, ${refused} refused` : ''}
          </dd>
        </dl>
        {renaming ? (
          <form
            class="renameform"
            onSubmit={(e) => {
              e.preventDefault()
              void agent?.rename(thread.thread_id, title)
              setRenaming(false)
            }}
          >
            <input value={title} aria-label="Thread title" maxLength={80} onInput={(e) => setTitle((e.target as HTMLInputElement).value)} />
            <button type="submit" class="btn small primary">
              Save
            </button>
          </form>
        ) : (
          <div class="dactions">
            <button type="button" class="btn small" onClick={() => (setTitle(thread.title), setRenaming(true))}>
              <Icon name="compose" /> Rename
            </button>
            <button
              type="button"
              class="btn small"
              onClick={async () => {
                await agent?.remove(thread.thread_id)
                navigate(href('agent'))
              }}
            >
              <Icon name="trash" /> Delete
            </button>
          </div>
        )}
      </Section>

      <Section title="Access in this thread">
        <div class="accessrows" role="radiogroup" aria-label="Access">
          {ACCESS_ORDER.map((a: AgentAccess) => (
            <button type="button" role="radio" aria-checked={thread.access === a} class={thread.access === a ? 'on' : ''} onClick={() => void agent?.setAccess(thread.thread_id, a)}>
              <span class="name">{ACCESS_INFO[a].label}</span>
              <span class="desc">{ACCESS_INFO[a].blurb}</span>
            </button>
          ))}
        </div>
      </Section>

      <Section title="Tools">
        <ul class="toollist">
          {TOOLS.map((t) => {
            const ok = mayCall(t.name, thread.access, enabled)
            return (
              <li class={ok.ok ? 'on' : 'off'} title={ok.ok ? t.summary : `Closed: ${ok.reason}`}>
                <Icon name={ok.ok ? 'check' : 'lock2'} />
                <span class="mono">{t.name}</span>
                <span class="lvl">{accessRank(t.access) === 0 ? '' : ACCESS_INFO[t.access].label}</span>
              </li>
            )
          })}
        </ul>
      </Section>

      {thread.mentions.length ? (
        <Section title="Context">
          <ul class="ctxlist">
            {thread.mentions.map((m) => (
              <li>
                <Chip tone="accent">{m.kind}</Chip>{' '}
                <a href={m.kind === 'workflow' ? href('workflows', m.id) : m.kind === 'run' ? href('runs', m.id) : m.kind === 'approval' ? href('approvals', m.id) : href('settings', 'connectors', m.id)}>{m.label}</a>
              </li>
            ))}
          </ul>
        </Section>
      ) : null}

      <Section title="Never, at any access">
        <ul class="never">
          {NEVER.map((n) => (
            <li>
              <Icon name="shield" />
              {n}
            </li>
          ))}
        </ul>
        <a class="small-text" href={href('settings', 'agent')}>
          Instructions and limits in Settings
        </a>
      </Section>
    </aside>
  )
}
