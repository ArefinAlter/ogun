// Shared components. States are always printed exactly (`waiting_for_user`), with colour as a second channel.

import type { ComponentChildren } from 'preact'
import { useEffect, useRef, useState } from 'preact/hooks'

import type { OgunError } from '../api/types'
import { shortHash } from '../lib/format'

export function StateLabel({ state }: { state: string }) {
  return <span class={`state s-${state}`}>{state}</span>
}

export function Hash({ value, short = false }: { value: string | undefined; short?: boolean }) {
  if (!value) return <span class="muted">–</span>
  return (
    <span class="hash" title={value}>
      {short ? shortHash(value) : value}
    </span>
  )
}

export function ErrorBox({ error, title }: { error: OgunError | undefined; title?: string }) {
  if (!error) return null
  return (
    <div class="notice error" role="alert">
      <div>
        {title ? <strong>{title}: </strong> : null}
        {error.message}
      </div>
      <div class="code">
        {error.code}
        {error.retryable ? ' · retryable' : ''}
      </div>
    </div>
  )
}

export function Notice({ kind = 'info', children }: { kind?: 'info' | 'warn' | 'error'; children: ComponentChildren }) {
  return <div class={`notice ${kind === 'info' ? '' : kind}`}>{children}</div>
}

export function Empty({ children }: { children: ComponentChildren }) {
  return <div class="empty">{children}</div>
}

export function Loading({ what = 'loading' }: { what?: string }) {
  return (
    <div class="loading" aria-live="polite">
      {what}…
    </div>
  )
}

/** Text that came from outside Ogun (a description, an agent's words, an operator message). */
export function Untrusted({ label, text }: { label: string; text: string | undefined }) {
  if (!text) return null
  return (
    <div class="untrusted">
      <span class="label">{label} · not verified by Ogun</span>
      <div class="body">{text}</div>
    </div>
  )
}

export function PageHead({ title, sub, children }: { title: ComponentChildren; sub?: ComponentChildren; children?: ComponentChildren }) {
  return (
    <div class="page-head">
      <div class="titles">
        <h1>{title}</h1>
        {sub ? <p class="sub">{sub}</p> : null}
      </div>
      {children ? <div class="actions">{children}</div> : null}
    </div>
  )
}

export function Kv({ rows }: { rows: [string, ComponentChildren][] }) {
  return (
    <dl class="kv">
      {rows.map(([k, v]) => (
        <>
          <dt>{k}</dt>
          <dd>{v}</dd>
        </>
      ))}
    </dl>
  )
}

export function Tabs<T extends string>({
  tabs,
  value,
  onChange,
}: {
  tabs: [T, string][]
  value: T
  onChange: (t: T) => void
}) {
  return (
    <div class="tabs" role="tablist">
      {tabs.map(([id, label]) => (
        <button type="button" role="tab" aria-selected={value === id} onClick={() => onChange(id)}>
          {label}
        </button>
      ))}
    </div>
  )
}

/** A modal dialog. Escape and the backdrop close it; focus moves into it. */
export function Dialog({
  title,
  onClose,
  children,
}: {
  title: string
  onClose: () => void
  children: ComponentChildren
}) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null
    ref.current?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => {
      window.removeEventListener('keydown', onKey)
      prev?.focus()
    }
  }, [onClose])
  return (
    <div
      class="dialog-back"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <div class="dialog" role="dialog" aria-modal="true" aria-label={title} tabIndex={-1} ref={ref}>
        <h2>{title}</h2>
        {children}
      </div>
    </div>
  )
}

/** Pretty-printed JSON. */
export function JsonBlock({ value, max = 200_000 }: { value: unknown; max?: number }) {
  let text = JSON.stringify(value, null, 2) ?? 'null'
  if (text.length > max) text = text.slice(0, max) + '\n…'
  return <pre class="code-block mono">{text}</pre>
}

// ---------------------------------------------------------------- controls shared by the settings pages

/** A toggle: a button with `role="switch"`, so it works with the keyboard and screen readers. */
export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange: (v: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button type="button" role="switch" aria-checked={checked} aria-label={label} disabled={disabled} class="switch" onClick={() => onChange(!checked)}>
      <span class="knob" />
    </button>
  )
}

/** A titled card of rows. */
export function SettingsGroup({ title, children, note }: { title?: ComponentChildren; children: ComponentChildren; note?: ComponentChildren }) {
  return (
    <section class="sgroup">
      {title ? <h2 class="sgroup-title">{title}</h2> : null}
      <div class="rows">{children}</div>
      {note ? <p class="sgroup-note">{note}</p> : null}
    </section>
  )
}

/** One row: a title and a description at the left, the control at the right. */
export function SettingRow({ title, desc, children, stack }: { title: ComponentChildren; desc?: ComponentChildren; children?: ComponentChildren; stack?: boolean }) {
  return (
    <div class={`row ${stack ? 'stack' : ''}`}>
      <div class="text">
        <div class="title">{title}</div>
        {desc ? <div class="desc">{desc}</div> : null}
      </div>
      {children ? <div class="control">{children}</div> : null}
    </div>
  )
}

export function Chip({ children, tone = '' }: { children: ComponentChildren; tone?: '' | 'ok' | 'warn' | 'bad' | 'info' | 'accent' }) {
  return <span class={`chip ${tone}`}>{children}</span>
}

/** Copies text to the clipboard; says "Copied" for two seconds. */
export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [done, setDone] = useState(false)
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text)
    } catch {
      const ta = document.createElement('textarea')
      ta.value = text
      document.body.appendChild(ta)
      ta.select()
      try {
        document.execCommand('copy')
      } catch {
        // Nothing more to try; the text stays selectable in the block beside the button.
      }
      ta.remove()
    }
    setDone(true)
    setTimeout(() => setDone(false), 2000)
  }
  return (
    <button type="button" class="btn small ghost" onClick={copy} aria-label={`${label} to the clipboard`}>
      {done ? 'Copied' : label}
    </button>
  )
}

/** A block of code or a command with a copy button. */
export function CodeCopy({ text, label }: { text: string; label?: string }) {
  return (
    <div class="codecopy">
      <pre class="mono">{text}</pre>
      <CopyButton text={text} label={label ?? 'Copy'} />
    </div>
  )
}

/** A link to another site. Inside the desktop shell navigation away is refused, so the address is shown to copy. */
export function ExternalLink({ href: url, children, desktop }: { href: string; children?: ComponentChildren; desktop: boolean }) {
  if (desktop)
    return (
      <span class="extlink">
        <span class="mono">{url}</span> <CopyButton text={url} />
      </span>
    )
  return (
    <a class="extlink" href={url} target="_blank" rel="noreferrer noopener">
      {children ?? url}
    </a>
  )
}

export function Segmented<T extends string>({ value, options, onChange, label }: { value: T; options: [T, string][]; onChange: (v: T) => void; label: string }) {
  return (
    <div class="segmented" role="radiogroup" aria-label={label}>
      {options.map(([id, text]) => (
        <button type="button" role="radio" aria-checked={value === id} class={value === id ? 'on' : ''} onClick={() => onChange(id)}>
          {text}
        </button>
      ))}
    </div>
  )
}

export function Slider({ value, min, max, step = 1, onChange, label, unit = '' }: { value: number; min: number; max: number; step?: number; onChange: (v: number) => void; label: string; unit?: string }) {
  return (
    <div class="slider">
      <input type="range" min={min} max={max} step={step} value={value} aria-label={label} onInput={(e) => onChange(Number((e.target as HTMLInputElement).value))} />
      <output class="mono">
        {value}
        {unit}
      </output>
    </div>
  )
}
