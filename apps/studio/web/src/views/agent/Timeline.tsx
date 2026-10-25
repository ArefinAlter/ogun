// The conversation: what the person wrote, what the agent said, the tools it called and the cards that came out of
// them. Tool calls are grouped under one line that opens to show each call with its input and the daemon's answer.

import { useEffect, useRef, useState } from 'preact/hooks'

import type { AgentItem, AgentThread } from '../../api/types'
import { fmtDuration } from '../../lib/format'
import { Chip, CopyButton, JsonBlock } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { Markdown } from '../../ui/Markdown'
import { CardView } from './Cards'

type ToolItem = Extract<AgentItem, { kind: 'tool' }>

function ToolRow({ t }: { t: ToolItem }) {
  const [open, setOpen] = useState(false)
  return (
    <li class={`tool ${t.state}`}>
      <button type="button" class="toolhead" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <span class="tstate" aria-label={t.state}>
          {t.state === 'running' ? <i class="spin" /> : t.state === 'ok' ? <Icon name="check" /> : t.state === 'denied' ? <Icon name="lock2" /> : <Icon name="x" />}
        </span>
        <span class="mono tname">{t.tool}</span>
        <span class="tsum">{t.summary}</span>
        {t.ms !== undefined ? <span class="tms mono">{fmtDuration(t.ms)}</span> : null}
        <Icon name="chevron" />
      </button>
      {open ? (
        <div class="toolbody">
          <div class="label">Input</div>
          <JsonBlock value={t.input ?? {}} />
          {t.output !== undefined ? (
            <>
              <div class="label">The daemon answered</div>
              <JsonBlock value={t.output} />
            </>
          ) : null}
        </div>
      ) : null}
    </li>
  )
}

function ToolGroup({ tools, live }: { tools: ToolItem[]; live: boolean }) {
  const running = tools.some((t) => t.state === 'running')
  const [open, setOpen] = useState<boolean | null>(null)
  const expanded = open ?? (running || live)
  const total = tools.reduce((n, t) => n + (t.ms ?? 0), 0)
  const denied = tools.filter((t) => t.state === 'denied').length
  return (
    <div class={`toolgroup ${expanded ? 'open' : ''}`}>
      <button type="button" class="grouphead" aria-expanded={expanded} onClick={() => setOpen(!expanded)}>
        <Icon name="wrench" />
        <span>
          {running ? 'Working' : 'Worked'} · {tools.length} tool call{tools.length === 1 ? '' : 's'}
          {!running && total ? ` · ${fmtDuration(total)}` : ''}
          {denied ? ` · ${denied} refused` : ''}
        </span>
        <Icon name="chevron" />
      </button>
      {expanded ? (
        <ul>
          {tools.map((t) => (
            <ToolRow t={t} />
          ))}
        </ul>
      ) : null}
    </div>
  )
}

function Message({ item }: { item: Extract<AgentItem, { kind: 'assistant' }> }) {
  return (
    <div class={`msg assistant ${item.streaming ? 'streaming' : ''}`}>
      <Markdown text={item.text} />
      {!item.streaming && item.text.length > 80 ? (
        <div class="msgfoot">
          <CopyButton text={item.text} label="Copy" />
        </div>
      ) : null}
    </div>
  )
}

export function Timeline({ thread }: { thread: AgentThread }) {
  const scroller = useRef<HTMLDivElement>(null)
  const [stick, setStick] = useState(true)
  const running = thread.state === 'running'
  const last = thread.items[thread.items.length - 1]
  const lastLen = last && last.kind === 'assistant' ? last.text.length : 0

  useEffect(() => {
    const el = scroller.current
    if (el && stick) el.scrollTop = el.scrollHeight
  }, [thread.items.length, lastLen, stick, running])

  const onScroll = () => {
    const el = scroller.current
    if (!el) return
    setStick(el.scrollHeight - el.scrollTop - el.clientHeight < 80)
  }

  // Group consecutive tool calls.
  const blocks: ({ kind: 'tools'; tools: ToolItem[]; key: string } | { kind: 'item'; item: AgentItem })[] = []
  for (const item of thread.items) {
    if (item.kind === 'tool') {
      const tail = blocks[blocks.length - 1]
      if (tail && tail.kind === 'tools') tail.tools.push(item)
      else blocks.push({ kind: 'tools', tools: [item], key: item.id })
    } else blocks.push({ kind: 'item', item })
  }
  const lastBlock = blocks[blocks.length - 1]
  const workingNow = running && !(last && last.kind === 'assistant' && last.streaming) && !(lastBlock && lastBlock.kind === 'tools')

  return (
    <div class="timeline-wrap">
    <div class="timeline" ref={scroller} onScroll={onScroll} role="log" aria-live="polite" aria-label="Conversation">
      <div class="column">
        {blocks.map((b) => {
          if (b.kind === 'tools') return <ToolGroup key={b.key} tools={b.tools} live={running && b === lastBlock} />
          const it = b.item
          switch (it.kind) {
            case 'user':
              return (
                <div class="msg user" key={it.id}>
                  <div class="bubble">
                    {it.mentions.length ? (
                      <div class="mentions">
                        {it.mentions.map((m) => (
                          <Chip tone="accent">@{m.label}</Chip>
                        ))}
                      </div>
                    ) : null}
                    {it.text}
                  </div>
                </div>
              )
            case 'assistant':
              return <Message key={it.id} item={it} />
            case 'card':
              return (
                <div class="cardrow" key={it.id}>
                  <CardView card={it.card} threadId={thread.thread_id} itemId={it.id} />
                </div>
              )
            case 'notice':
              return (
                <div class={`inlinenote ${it.tone}`} key={it.id}>
                  {it.text}
                </div>
              )
            default:
              return null
          }
        })}
        {workingNow ? (
          <div class="working" aria-live="polite">
            <i class="dots" />
            <span>Working…</span>
          </div>
        ) : null}
      </div>
    </div>
      {!stick && running ? (
        <button type="button" class="jump" onClick={() => setStick(true)}>
          <Icon name="down" /> Latest
        </button>
      ) : null}
    </div>
  )
}
