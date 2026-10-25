// Renders the tree of `lib/markdown.ts`. Text is placed as text, so nothing an agent writes becomes markup.

import type { ComponentChildren } from 'preact'

import { parseMarkdown } from '../lib/markdown'
import type { Block, Inline } from '../lib/markdown'

function Inlines({ nodes }: { nodes: Inline[] }) {
  return (
    <>
      {nodes.map((n) => {
        switch (n.t) {
          case 'text':
            return n.v
          case 'code':
            return <code>{n.v}</code>
          case 'strong':
            return (
              <strong>
                <Inlines nodes={n.c} />
              </strong>
            )
          case 'em':
            return (
              <em>
                <Inlines nodes={n.c} />
              </em>
            )
          case 'link':
            return n.internal ? (
              <a href={n.href}>
                <Inlines nodes={n.c} />
              </a>
            ) : (
              <a href={n.href} target="_blank" rel="noreferrer noopener">
                <Inlines nodes={n.c} />
              </a>
            )
        }
      })}
    </>
  )
}

function BlockView({ b }: { b: Block }): ComponentChildren {
  switch (b.t) {
    case 'p':
      return (
        <p>
          <Inlines nodes={b.c} />
        </p>
      )
    case 'h': {
      const H = `h${b.level + 1}` as 'h2' | 'h3' | 'h4'
      return (
        <H class="md-h">
          <Inlines nodes={b.c} />
        </H>
      )
    }
    case 'ul':
      return (
        <ul>
          {b.items.map((i) => (
            <li>
              <Inlines nodes={i} />
            </li>
          ))}
        </ul>
      )
    case 'ol':
      return (
        <ol>
          {b.items.map((i) => (
            <li>
              <Inlines nodes={i} />
            </li>
          ))}
        </ol>
      )
    case 'code':
      return <pre class="code-block mono">{b.v}</pre>
    case 'quote':
      return (
        <blockquote>
          <Inlines nodes={b.c} />
        </blockquote>
      )
    case 'hr':
      return <hr />
    case 'table':
      return (
        <div class="md-table">
          <table>
            <thead>
              <tr>
                {b.head.map((h) => (
                  <th>
                    <Inlines nodes={h} />
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {b.rows.map((r) => (
                <tr>
                  {r.map((c) => (
                    <td>
                      <Inlines nodes={c} />
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )
  }
}

export function Markdown({ text }: { text: string }) {
  return (
    <div class="md">
      {parseMarkdown(text).map((b) => (
        <BlockView b={b} />
      ))}
    </div>
  )
}
