// Shared components. States are always printed exactly (`waiting_for_user`), with colour as a second channel.

import type { ComponentChildren } from 'preact'
import { useEffect, useRef } from 'preact/hooks'

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

export function PageHead({ title, children }: { title: ComponentChildren; children?: ComponentChildren }) {
  return (
    <div class="page-head">
      <h1>{title}</h1>
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
