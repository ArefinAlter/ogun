// A small, safe Markdown reader for agent messages. An agent's words are untrusted text, so this never produces HTML:
// it returns a tree that the interface renders node by node. Raw HTML stays text, and a link is kept only when it is
// an address inside Studio (`#/...`) or an `https` address without credentials; anything else is shown as plain text.
//
// Supported: paragraphs, `#` to `###` headings, `-`, `*` and `1.` lists, fenced code, `>` quotes, `---`, pipe tables,
// and inside text: `code`, **strong**, *emphasis* and [label](address). No imports, so `node --test` can load it.

export type Inline =
  | { t: 'text'; v: string }
  | { t: 'code'; v: string }
  | { t: 'strong'; c: Inline[] }
  | { t: 'em'; c: Inline[] }
  | { t: 'link'; href: string; internal: boolean; c: Inline[] }

export type Block =
  | { t: 'p'; c: Inline[] }
  | { t: 'h'; level: 1 | 2 | 3; c: Inline[] }
  | { t: 'ul'; items: Inline[][] }
  | { t: 'ol'; items: Inline[][] }
  | { t: 'code'; lang: string; v: string }
  | { t: 'quote'; c: Inline[] }
  | { t: 'table'; head: Inline[][]; rows: Inline[][][] }
  | { t: 'hr' }

/** The address of a link if it is safe to follow, else null. */
export function safeHref(url: string): { href: string; internal: boolean } | null {
  const u = url.trim()
  if (u.length > 2000) return null
  if (u.startsWith('#/') && !/[\u0000-\u001f\s<>"]/.test(u)) return { href: u, internal: true }
  if (u.startsWith('https://') && !/[\u0000-\u001f\s<>"]/.test(u)) {
    try {
      const parsed = new URL(u)
      if (parsed.username || parsed.password) return null
      return { href: parsed.href, internal: false }
    } catch {
      return null
    }
  }
  return null
}

const PUNCT = '\\`*_{}[]()#+-.!|>~'

export function parseInline(src: string): Inline[] {
  const out: Inline[] = []
  let text = ''
  const flush = () => {
    if (text) out.push({ t: 'text', v: text })
    text = ''
  }
  let i = 0
  while (i < src.length) {
    const c = src[i] as string
    if (c === '\\' && i + 1 < src.length && PUNCT.includes(src[i + 1] as string)) {
      text += src[i + 1]
      i += 2
      continue
    }
    if (c === '`') {
      const end = src.indexOf('`', i + 1)
      if (end > i + 1) {
        flush()
        out.push({ t: 'code', v: src.slice(i + 1, end) })
        i = end + 1
        continue
      }
    }
    if (c === '*' && src[i + 1] === '*') {
      const end = src.indexOf('**', i + 2)
      if (end > i + 2) {
        flush()
        out.push({ t: 'strong', c: parseInline(src.slice(i + 2, end)) })
        i = end + 2
        continue
      }
    }
    if ((c === '*' || c === '_') && src[i + 1] !== ' ' && src[i + 1] !== c) {
      const end = src.indexOf(c, i + 1)
      const prev = i === 0 ? ' ' : (src[i - 1] as string)
      if (end > i + 1 && src[end - 1] !== ' ' && (c === '*' || !/\w/.test(prev))) {
        flush()
        out.push({ t: 'em', c: parseInline(src.slice(i + 1, end)) })
        i = end + 1
        continue
      }
    }
    if (c === '[') {
      const close = src.indexOf('](', i + 1)
      if (close > i) {
        const end = src.indexOf(')', close + 2)
        if (end > close) {
          const label = src.slice(i + 1, close)
          const url = src.slice(close + 2, end)
          const safe = safeHref(url)
          flush()
          if (safe) out.push({ t: 'link', href: safe.href, internal: safe.internal, c: parseInline(label) })
          else out.push({ t: 'text', v: `${label} (${url})` })
          i = end + 1
          continue
        }
      }
    }
    text += c
    i++
  }
  flush()
  return out
}

function splitRow(line: string): string[] {
  let l = line.trim()
  if (l.startsWith('|')) l = l.slice(1)
  if (l.endsWith('|')) l = l.slice(0, -1)
  const cells: string[] = []
  let cur = ''
  let inCode = false
  for (let i = 0; i < l.length; i++) {
    const ch = l[i] as string
    if (ch === '`') inCode = !inCode
    if (ch === '\\' && l[i + 1] === '|') {
      cur += '|'
      i++
      continue
    }
    if (ch === '|' && !inCode) {
      cells.push(cur.trim())
      cur = ''
    } else cur += ch
  }
  cells.push(cur.trim())
  return cells
}

const isSeparator = (line: string) => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(line)

export function parseMarkdown(src: string): Block[] {
  const lines = src.replace(/\r\n?/g, '\n').split('\n')
  const blocks: Block[] = []
  let i = 0
  const para: string[] = []
  const flushPara = () => {
    if (para.length) blocks.push({ t: 'p', c: parseInline(para.join(' ')) })
    para.length = 0
  }
  while (i < lines.length) {
    const line = lines[i] as string
    const fence = /^```\s*([A-Za-z0-9_+-]*)\s*$/.exec(line)
    if (fence) {
      flushPara()
      const body: string[] = []
      i++
      while (i < lines.length && !/^```\s*$/.test(lines[i] as string)) {
        body.push(lines[i] as string)
        i++
      }
      i++ // the closing fence; an unclosed fence ends with the text, as while streaming
      blocks.push({ t: 'code', lang: fence[1] ?? '', v: body.join('\n') })
      continue
    }
    if (!line.trim()) {
      flushPara()
      i++
      continue
    }
    const h = /^(#{1,3})\s+(.*)$/.exec(line)
    if (h) {
      flushPara()
      blocks.push({ t: 'h', level: (h[1] as string).length as 1 | 2 | 3, c: parseInline((h[2] as string).trim()) })
      i++
      continue
    }
    if (/^\s*(-{3,}|\*{3,})\s*$/.test(line)) {
      flushPara()
      blocks.push({ t: 'hr' })
      i++
      continue
    }
    if (line.trimStart().startsWith('|') && i + 1 < lines.length && isSeparator(lines[i + 1] as string)) {
      flushPara()
      const head = splitRow(line).map(parseInline)
      i += 2
      const rows: Inline[][][] = []
      while (i < lines.length && (lines[i] as string).trimStart().startsWith('|')) {
        rows.push(splitRow(lines[i] as string).map(parseInline))
        i++
      }
      blocks.push({ t: 'table', head, rows })
      continue
    }
    const ul = /^\s*[-*]\s+(.*)$/.exec(line)
    const ol = /^\s*\d+[.)]\s+(.*)$/.exec(line)
    if (ul || ol) {
      flushPara()
      const ordered = !!ol
      const items: Inline[][] = []
      while (i < lines.length) {
        const m = ordered ? /^\s*\d+[.)]\s+(.*)$/.exec(lines[i] as string) : /^\s*[-*]\s+(.*)$/.exec(lines[i] as string)
        if (!m) break
        let item = m[1] as string
        i++
        // A continuation line is indented and belongs to the item above.
        while (i < lines.length && /^\s{2,}\S/.test(lines[i] as string) && !/^\s*([-*]|\d+[.)])\s+/.test(lines[i] as string)) {
          item += ' ' + (lines[i] as string).trim()
          i++
        }
        items.push(parseInline(item))
      }
      blocks.push(ordered ? { t: 'ol', items } : { t: 'ul', items })
      continue
    }
    if (line.startsWith('>')) {
      flushPara()
      const q: string[] = []
      while (i < lines.length && (lines[i] as string).startsWith('>')) {
        q.push((lines[i] as string).replace(/^>\s?/, ''))
        i++
      }
      blocks.push({ t: 'quote', c: parseInline(q.join(' ')) })
      continue
    }
    para.push(line.trim())
    i++
  }
  flushPara()
  return blocks
}
