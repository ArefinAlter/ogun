// A small anchored menu that opens upward or downward from its button, closes on Escape, on an outside press and
// after a choice. Used for the composer's pickers.

import type { ComponentChildren } from 'preact'
import { useEffect, useRef, useState } from 'preact/hooks'

export function Popover({
  label,
  trigger,
  children,
  up = true,
  align = 'left',
  disabled,
  title,
}: {
  label: string
  trigger: ComponentChildren
  children: (close: () => void) => ComponentChildren
  up?: boolean
  align?: 'left' | 'right'
  disabled?: boolean
  title?: string
}) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return undefined
    const onDown = (e: MouseEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        setOpen(false)
      }
    }
    window.addEventListener('mousedown', onDown)
    window.addEventListener('keydown', onKey, true)
    return () => {
      window.removeEventListener('mousedown', onDown)
      window.removeEventListener('keydown', onKey, true)
    }
  }, [open])
  return (
    <div class="popover" ref={root}>
      <button type="button" class="chipbtn" aria-haspopup="menu" aria-expanded={open} aria-label={label} title={title ?? label} disabled={disabled} onClick={() => setOpen((v) => !v)}>
        {trigger}
      </button>
      {open ? (
        <div class={`popmenu ${up ? 'up' : 'down'} ${align}`} role="menu">
          {children(() => setOpen(false))}
        </div>
      ) : null}
    </div>
  )
}
