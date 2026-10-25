// The composer: where a person gives the agent a task. Enter sends, Shift+Enter starts a new line, `@` points at a
// workflow, a run, an approval or a connector, `/` offers ready-made requests. The pickers set the model and how much
// the agent may do in this thread; neither can grant it a tool that does not exist (lib/agentTools.ts).

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import type { AgentAccess, AgentModel, Mention } from '../../api/types'
import { useCatalog } from '../../connectors'
import { ACCESS_INFO, ACCESS_ORDER } from '../../lib/agentTools'
import { useAsync, useBackend } from '../../lib/hooks'
import { Chip } from '../../ui/components'
import { Icon } from '../../ui/icons'
import type { IconName } from '../../ui/icons'
import { Popover } from '../../ui/Popover'

const drafts = new Map<string, { text: string; mentions: Mention[] }>()

const COMMANDS: { name: string; hint: string; text: string }[] = [
  { name: 'status', hint: 'What is running and what is waiting', text: 'What is running, and what is waiting for a person?' },
  { name: 'failures', hint: 'Why did the latest failure happen', text: 'Why did the latest failed run fail?' },
  { name: 'approvals', hint: 'What each pending request asks for', text: 'What is waiting for a person, and what does each request ask for?' },
  { name: 'backfill', hint: 'Plan a backfill', text: 'Backfill @trips-backfill since 2019' },
  { name: 'draft', hint: 'Draft a new workflow', text: 'Draft a daily workflow that lists trips per station from the open-trips archive' },
  { name: 'stations', hint: 'Stations to rebalance', text: 'Which stations are likely to run empty or full today?' },
  { name: 'connectors', hint: 'What a system needs', text: 'Which connectors would I need to send alerts to Datadog and DingTalk?' },
]

interface MenuItem {
  key: string
  label: string
  meta?: string
  icon: IconName
  /** What to put in the text, and the mention to record, if any. */
  insert: string
  mention?: Mention
}

export function Composer({
  draftKey,
  busy,
  access,
  onAccess,
  model,
  onModel,
  models,
  onSend,
  onStop,
  lockModel,
  placeholder,
  disabledReason,
}: {
  draftKey: string
  busy: boolean
  access: AgentAccess
  onAccess: (a: AgentAccess) => void
  model: string
  onModel: (id: string) => void
  models: AgentModel[]
  onSend: (text: string, mentions: Mention[]) => void
  onStop: () => void
  /** A thread keeps the model it started with. */
  lockModel?: boolean
  placeholder?: string
  disabledReason?: string
}) {
  const backend = useBackend()
  const catalog = useCatalog()
  const saved = drafts.get(draftKey)
  const [text, setText] = useState(saved?.text ?? '')
  const [mentions, setMentions] = useState<Mention[]>(saved?.mentions ?? [])
  const [menu, setMenu] = useState<null | { kind: '@' | '/'; query: string; start: number }>(null)
  const [sel, setSel] = useState(0)
  const area = useRef<HTMLTextAreaElement>(null)

  // A different thread, a different draft.
  useEffect(() => {
    const d = drafts.get(draftKey)
    setText(d?.text ?? '')
    setMentions(d?.mentions ?? [])
    setMenu(null)
  }, [draftKey])
  useEffect(() => {
    drafts.set(draftKey, { text, mentions })
  }, [draftKey, text, mentions])
  useEffect(() => {
    const el = area.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, 220)}px`
  }, [text])
  useEffect(() => {
    area.current?.focus()
  }, [draftKey])

  const workflows = useAsync(() => backend.workflowsList().catch(() => ({ workflows: [] })), [backend])
  const runs = useAsync(() => backend.runsList({ limit: 6 }).catch(() => ({ runs: [] })), [backend])
  const approvals = useAsync(() => backend.approvalsList('pending').catch(() => ({ approvals: [] })), [backend])

  const items = useMemo<MenuItem[]>(() => {
    if (!menu) return []
    const q = menu.query.toLowerCase()
    if (menu.kind === '/') {
      return COMMANDS.filter((c) => c.name.startsWith(q)).map((c) => ({ key: c.name, label: `/${c.name}`, meta: c.hint, icon: 'sparkle' as IconName, insert: c.text }))
    }
    const out: MenuItem[] = []
    for (const w of workflows.data?.workflows ?? []) if (w.name.includes(q)) out.push({ key: 'w:' + w.name, label: w.name, meta: `workflow v${w.latest_version}`, icon: 'dag', insert: `@${w.name} `, mention: { kind: 'workflow', id: w.name, label: w.name } })
    for (const r of runs.data?.runs ?? []) if (`${r.workflow} ${r.run_id}`.toLowerCase().includes(q)) out.push({ key: 'r:' + r.run_id, label: `${r.workflow} run`, meta: r.state, icon: 'play', insert: `@${r.run_id.slice(0, 14)} `, mention: { kind: 'run', id: r.run_id, label: r.run_id.slice(0, 14) } })
    for (const a of approvals.data?.approvals ?? []) if (`${a.kind} ${a.workflow}`.includes(q)) out.push({ key: 'a:' + a.approval_id, label: `${a.kind.replace('_', ' ')} for ${a.workflow}`, meta: 'pending approval', icon: 'seal', insert: `@${a.workflow}-approval `, mention: { kind: 'approval', id: a.approval_id, label: `${a.workflow}-approval` } })
    if (q.length >= 2) for (const m of (catalog ?? []).filter((x) => `${x.id} ${x.name}`.toLowerCase().includes(q)).slice(0, 4)) out.push({ key: 'c:' + m.id, label: m.name, meta: 'connector', icon: 'plug', insert: `@${m.id} `, mention: { kind: 'connector', id: m.id, label: m.id } })
    return out.slice(0, 8)
  }, [menu, workflows.data, runs.data, approvals.data, catalog])
  useEffect(() => setSel(0), [menu?.query, menu?.kind])

  const detect = (value: string, caret: number) => {
    const before = value.slice(0, caret)
    const at = /(^|\s)@([A-Za-z0-9._-]*)$/.exec(before)
    if (at) return setMenu({ kind: '@', query: at[2] ?? '', start: caret - (at[2]?.length ?? 0) - 1 })
    const slash = /^\/([a-z]*)$/.exec(before)
    if (slash) return setMenu({ kind: '/', query: slash[1] ?? '', start: 0 })
    setMenu(null)
  }

  const choose = (it: MenuItem) => {
    if (!menu) return
    const el = area.current
    const caret = el?.selectionStart ?? text.length
    const next = text.slice(0, menu.start) + it.insert + text.slice(caret)
    setText(next)
    if (it.mention) {
      const m = it.mention
      setMentions((cur) => (cur.some((x) => x.kind === m.kind && x.id === m.id) ? cur : [...cur, m]))
    }
    setMenu(null)
    requestAnimationFrame(() => {
      if (!el) return
      el.focus()
      const pos = menu.start + it.insert.length
      el.setSelectionRange(pos, pos)
    })
  }

  const send = () => {
    const t = text.trim()
    if (!t || busy || disabledReason) return
    const used = mentions.filter((m) => t.includes('@' + m.label))
    onSend(t, used)
    setText('')
    setMentions([])
    setMenu(null)
    drafts.delete(draftKey)
  }

  const onKeyDown = (e: KeyboardEvent) => {
    if (menu && items.length) {
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        setSel((s) => Math.min(s + 1, items.length - 1))
        return
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault()
        setSel((s) => Math.max(s - 1, 0))
        return
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault()
        const it = items[sel]
        if (it) choose(it)
        return
      }
    }
    if (e.key === 'Escape') {
      if (menu) {
        e.preventDefault()
        setMenu(null)
      }
      return
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault()
      send()
    }
  }

  const model_ = models.find((m) => m.id === model)
  return (
    <div class="composer-wrap">
      <div class={`composer ${busy ? 'busy' : ''}`}>
        {mentions.length ? (
          <div class="cmp-mentions">
            {mentions.map((m) => (
              <span class="chip accent removable">
                @{m.label}
                <button type="button" aria-label={`Remove ${m.label}`} onClick={() => setMentions((cur) => cur.filter((x) => x !== m))}>
                  <Icon name="x" />
                </button>
              </span>
            ))}
          </div>
        ) : null}
        <textarea
          ref={area}
          rows={2}
          value={text}
          placeholder={placeholder ?? 'Ask anything about your data pipelines, @point at a workflow or run, or / for ready-made requests'}
          aria-label="Message to the agent"
          disabled={!!disabledReason}
          onInput={(e) => {
            const el = e.target as HTMLTextAreaElement
            setText(el.value)
            detect(el.value, el.selectionStart)
          }}
          onKeyDown={onKeyDown}
          onClick={(e) => detect((e.target as HTMLTextAreaElement).value, (e.target as HTMLTextAreaElement).selectionStart)}
        />
        {menu && items.length ? (
          <div class="mentionmenu" role="listbox" aria-label={menu.kind === '@' ? 'Attach context' : 'Ready-made requests'}>
            {items.map((it, i) => (
              <button type="button" role="option" aria-selected={i === sel} class={i === sel ? 'on' : ''} onMouseEnter={() => setSel(i)} onMouseDown={(e) => (e.preventDefault(), choose(it))}>
                <Icon name={it.icon} />
                <span class="name">{it.label}</span>
                {it.meta ? <span class="meta">{it.meta}</span> : null}
              </button>
            ))}
          </div>
        ) : null}
        <div class="cmp-bar">
          <Popover label={`Model: ${model_?.label ?? model}`} disabled={!!lockModel} title={lockModel ? 'A thread keeps the model it started with' : 'Model'} trigger={<><Icon name="sparkle" /> <span class="cbl">{model_?.label ?? model}</span> <Icon name="chevron" /></>}>
            {(close) => (
              <>
                <div class="pophead">Model</div>
                {models.map((m) => (
                  <button type="button" role="menuitemradio" aria-checked={m.id === model} disabled={!m.available} class={m.id === model ? 'on' : ''} onClick={() => (onModel(m.id), close())}>
                    <span class="name">{m.label}</span>
                    <span class="meta">{m.provider}</span>
                    <span class="desc">{m.note ?? ''}</span>
                  </button>
                ))}
              </>
            )}
          </Popover>
          <span class="vsep" />
          <Popover label={`Access: ${ACCESS_INFO[access].label}`} trigger={<><Icon name={access === 'read' ? 'eye' : access === 'author' ? 'compose' : 'play'} /> <span class="cbl">{ACCESS_INFO[access].label}</span> <Icon name="chevron" /></>}>
            {(close) => (
              <>
                <div class="pophead">What the agent may do in this thread</div>
                {ACCESS_ORDER.map((a) => (
                  <button type="button" role="menuitemradio" aria-checked={a === access} class={a === access ? 'on' : ''} onClick={() => (onAccess(a), close())}>
                    <span class="name">{ACCESS_INFO[a].label}</span>
                    <span class="desc">{ACCESS_INFO[a].blurb}</span>
                  </button>
                ))}
                <div class="popfoot">It can never decide an approval, read a secret value or run a shell command.</div>
              </>
            )}
          </Popover>
          <span class="spacer" />
          <button
            type="button"
            class="icon-btn"
            aria-label="Attach context with @"
            title="Attach a workflow, run, approval or connector"
            onClick={() => {
              const el = area.current
              if (!el) return
              const next = text + (text && !/\s$/.test(text) ? ' ' : '') + '@'
              setText(next)
              requestAnimationFrame(() => {
                el.focus()
                el.setSelectionRange(next.length, next.length)
                detect(next, next.length)
              })
            }}
          >
            <Icon name="attach" />
          </button>
          {busy ? (
            <button type="button" class="sendbtn stop" aria-label="Stop the agent" title="Stop" onClick={onStop}>
              <Icon name="stop" />
            </button>
          ) : (
            <button type="button" class="sendbtn" aria-label="Send" title="Send (Enter)" disabled={!text.trim() || !!disabledReason} onClick={send}>
              <Icon name="send" />
            </button>
          )}
        </div>
      </div>
      <div class="cmp-note">
        {disabledReason ? <Chip tone="warn">{disabledReason}</Chip> : null}
        <span>The agent's words are not verified by Ogun. It cannot decide an approval.</span>
      </div>
    </div>
  )
}
