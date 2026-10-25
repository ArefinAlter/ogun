// The cards of an agent thread. Prose is the agent's; a card is made from what the daemon answered (a validation
// result, a plan, a run, an approval request) and says so. The buttons on a card are pressed by a person and run with
// that person's own role. There is deliberately no button that decides an approval: a request links to the Approvals
// page, where the facts come first and the decision has its own flow.

import { useState } from 'preact/hooks'

import type { AgentAction, AgentApprovalRef, AgentCard } from '../../api/types'
import { asOgunError } from '../../api'
import { categoryLabel, MATURITY, useCatalog } from '../../connectors'
import { fmtAgo } from '../../lib/format'
import { idempotencyKey } from '../../lib/format'
import { href, useAsync, useBackend, useInfo, useLive } from '../../lib/hooks'
import { Chip, CopyButton, ErrorBox, Hash, JsonBlock, StateLabel } from '../../ui/components'
import { Icon } from '../../ui/icons'
import type { IconName } from '../../ui/icons'
import type { OgunError } from '../../api/types'

function Shell({ icon, title, chips, children, foot }: { icon: IconName; title: string; chips?: preact.ComponentChildren; children?: preact.ComponentChildren; foot?: preact.ComponentChildren }) {
  return (
    <section class="acard">
      <header>
        <span class="well">
          <Icon name={icon} />
        </span>
        <h3>{title}</h3>
        <span class="chips">{chips}</span>
      </header>
      {children ? <div class="body">{children}</div> : null}
      {foot ? <footer>{foot}</footer> : null}
    </section>
  )
}

function RunCard({ runId, workflow }: { runId: string; workflow: string }) {
  const backend = useBackend()
  const run = useAsync(() => backend.runsGet(runId), [backend, runId])
  useLive(() => run.reload(), runId)
  const nodes = run.data?.nodes ?? []
  const done = nodes.filter((n) => n.state === 'succeeded' || n.state === 'skipped').length
  const pct = nodes.length ? Math.round((done / nodes.length) * 100) : 0
  return (
    <Shell icon="play" title={`Run of ${workflow}`} chips={run.data ? <StateLabel state={run.data.state} /> : null} foot={<a class="btn small" href={href('runs', runId)}>Open the run</a>}>
      <div class="muted mono small-text">{runId}</div>
      {nodes.length ? (
        <div class="meter" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label="Node instances finished">
          <span style={{ width: `${pct}%` }} />
        </div>
      ) : null}
      {run.data ? (
        <div class="muted small-text">
          {run.data.partition_count} partition{run.data.partition_count === 1 ? '' : 's'}, {done} of {nodes.length} node instances finished, started {fmtAgo(run.data.created_at)}
        </div>
      ) : null}
    </Shell>
  )
}

function ApprovalsCard({ approvals }: { approvals: AgentApprovalRef[] }) {
  return (
    <Shell icon="seal" title="Waiting for a person" chips={<Chip tone="warn">{approvals.length}</Chip>} foot={<span class="muted small-text">Decided only by a person, on the Approvals page or a terminal. Nothing in this conversation can decide one.</span>}>
      <ul class="arows">
        {approvals.map((a) => (
          <li>
            <a href={href('approvals', a.approval_id)}>
              <Chip tone="accent">{a.kind.replace('_', ' ')}</Chip> <strong>{a.workflow}</strong>
              {a.node ? <span class="mono"> · {a.node}</span> : null}
            </a>
            <span class="muted small-text">requested by {a.requested_by}</span>
          </li>
        ))}
      </ul>
    </Shell>
  )
}

function TableCard({ title, columns, rows, source }: { title: string; columns: string[]; rows: string[][]; source?: string }) {
  return (
    <Shell icon="matrix" title={title} chips={<Chip>{rows.length} rows</Chip>} foot={source ? <span class="muted small-text">From {source}</span> : null}>
      <div class="md-table">
        <table>
          <thead>
            <tr>
              {columns.map((c) => (
                <th>{c}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr>
                {r.map((c) => (
                  <td class="mono">{c}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Shell>
  )
}

function ConnectorCard({ id, why }: { id: string; why: string }) {
  const catalog = useCatalog()
  const m = catalog?.find((x) => x.id === id)
  if (!m) return null
  return (
    <Shell
      icon="plug"
      title={m.name}
      chips={
        <>
          <Chip tone={m.maturity === 'contract' ? 'warn' : 'ok'}>{MATURITY[m.maturity].label}</Chip>
          <Chip>{categoryLabel(m.category)}</Chip>
        </>
      }
      foot={
        <>
          <a class="btn small" href={href('settings', 'connectors', m.id)}>
            Open the connector
          </a>
          <a class="btn small" href={href('settings', 'connections', m.id.replace(/[^a-z0-9_-]/g, '-').slice(0, 40), { connector: m.id })}>
            Add a connection
          </a>
        </>
      }
    >
      <div class="small-text">{why}</div>
      {m.egress.domains.length ? (
        <div class="muted small-text">
          Reaches <span class="mono">{m.egress.domains.slice(0, 3).join(', ')}</span>
        </div>
      ) : null}
    </Shell>
  )
}

function PlanCard({ title, steps }: { title: string; steps: { text: string; state: 'todo' | 'doing' | 'done' }[] }) {
  return (
    <Shell icon="check" title={title}>
      <ol class="plan">
        {steps.map((s) => (
          <li class={s.state}>
            <span class="mark" aria-label={s.state}>
              {s.state === 'done' ? <Icon name="check" /> : null}
            </span>
            {s.text}
          </li>
        ))}
      </ol>
    </Shell>
  )
}

function useAct(threadId: string, itemId: string) {
  const backend = useBackend()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  const run = async (action: AgentAction) => {
    setBusy(true)
    setError(undefined)
    try {
      await backend.agent?.act(threadId, itemId, action, idempotencyKey('agent-' + action))
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  return { busy, error, run }
}

function ProposalCard({ card, threadId, itemId }: { card: Extract<AgentCard, { type: 'proposal' }>; threadId: string; itemId: string }) {
  const { info } = useInfo()
  const act = useAct(threadId, itemId)
  const [open, setOpen] = useState(false)
  const errors = card.diagnostics.filter((d) => d.severity === 'error')
  const warnings = card.diagnostics.filter((d) => d.severity !== 'error')
  return (
    <Shell
      icon="dag"
      title={`Proposed workflow: ${card.workflow.workflow} v${card.workflow.version}`}
      chips={
        <>
          <Chip tone={card.valid ? 'ok' : 'bad'}>{card.valid ? 'valid' : `${errors.length} error${errors.length === 1 ? '' : 's'}`}</Chip>
          {card.stored ? <Chip tone="ok">stored as v{card.stored.version}</Chip> : <Chip>not stored</Chip>}
        </>
      }
      foot={
        <>
          {card.stored ? (
            <a class="btn small primary" href={href('workflows', card.workflow.workflow)}>
              Open the workflow
            </a>
          ) : (
            <button type="button" class="btn small primary" disabled={!card.valid || act.busy || !info?.canWrite} title={info?.canWrite ? 'Stores this exact document as a new version, as you' : 'This connection cannot store workflows'} onClick={() => void act.run('store')}>
              Store as you
            </button>
          )}
          <button type="button" class="btn small" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
            {open ? 'Hide the document' : 'Show the document'}
          </button>
          <CopyButton text={JSON.stringify(card.workflow, null, 2)} label="Copy JSON" />
        </>
      }
    >
      {card.workflow.description ? <div class="small-text">{card.workflow.description}</div> : null}
      <dl class="kvs">
        <dt>hash</dt>
        <dd>
          <Hash value={card.hash} short />
        </dd>
        <dt>nodes</dt>
        <dd class="mono">{card.workflow.nodes.map((n) => `${n.id}:${n.op}`).join('  ')}</dd>
      </dl>
      {card.diagnostics.length ? (
        <ul class="diags">
          {[...errors, ...warnings].slice(0, 6).map((d) => (
            <li class={d.severity}>
              <Chip tone={d.severity === 'error' ? 'bad' : 'warn'}>{d.severity}</Chip> <span class="mono">{d.code}</span> {d.message}
            </li>
          ))}
        </ul>
      ) : null}
      {card.needs.length ? (
        <div class="needs">
          <div class="label">Waits for a person</div>
          <ul>
            {card.needs.map((n) => (
              <li>{n}</li>
            ))}
          </ul>
        </div>
      ) : null}
      {open ? <JsonBlock value={card.workflow} /> : null}
      <ErrorBox error={act.error} />
    </Shell>
  )
}

function BackfillCard({ card, threadId, itemId }: { card: Extract<AgentCard, { type: 'backfill' }>; threadId: string; itemId: string }) {
  const { info } = useInfo()
  const act = useAct(threadId, itemId)
  return (
    <Shell
      icon="matrix"
      title={`Backfill plan: ${card.workflow}`}
      chips={card.started ? <Chip tone="ok">started</Chip> : card.to_run === 0 ? <Chip>nothing to run</Chip> : <Chip tone="warn">not started</Chip>}
      foot={
        card.started ? (
          <a class="btn small primary" href={href('runs', card.started.run_id)}>
            Open the run
          </a>
        ) : card.to_run > 0 ? (
          <button type="button" class="btn small primary" disabled={act.busy || !info?.canWrite} title={info?.canWrite ? 'Starts exactly this plan, as you' : 'This connection cannot start runs'} onClick={() => void act.run('start_backfill')}>
            Start {card.to_run} partition{card.to_run === 1 ? '' : 's'} as you
          </button>
        ) : null
      }
    >
      <dl class="kvs">
        <dt>range</dt>
        <dd class="mono">
          {card.from} to {card.to}
        </dd>
        <dt>to run</dt>
        <dd>{card.to_run}</dd>
        <dt>up to date</dt>
        <dd>{card.up_to_date}, and a done partition never runs again</dd>
      </dl>
      <ErrorBox error={act.error} />
    </Shell>
  )
}

function RetryCard({ card, threadId, itemId }: { card: Extract<AgentCard, { type: 'retry' }>; threadId: string; itemId: string }) {
  const { info } = useInfo()
  const act = useAct(threadId, itemId)
  return (
    <Shell
      icon="refresh"
      title={`Retry ${card.node} of ${card.workflow}`}
      chips={card.started ? <Chip tone="ok">started</Chip> : <Chip tone="warn">failed</Chip>}
      foot={
        card.started ? (
          <a class="btn small primary" href={href('runs', card.started.run_id)}>
            Open the new run
          </a>
        ) : (
          <button type="button" class="btn small primary" disabled={act.busy || !info?.canWrite} onClick={() => void act.run('retry')}>
            Retry the partition as you
          </button>
        )
      }
    >
      <dl class="kvs">
        <dt>failed run</dt>
        <dd>
          <a class="mono" href={href('runs', card.run_id)}>
            {card.run_id}
          </a>
        </dd>
        <dt>cause</dt>
        <dd class="mono">{card.cause}</dd>
      </dl>
      <ErrorBox error={act.error} />
    </Shell>
  )
}

export function CardView({ card, threadId, itemId }: { card: AgentCard; threadId: string; itemId: string }) {
  switch (card.type) {
    case 'plan':
      return <PlanCard title={card.title} steps={card.steps} />
    case 'proposal':
      return <ProposalCard card={card} threadId={threadId} itemId={itemId} />
    case 'backfill':
      return <BackfillCard card={card} threadId={threadId} itemId={itemId} />
    case 'retry':
      return <RetryCard card={card} threadId={threadId} itemId={itemId} />
    case 'approvals':
      return <ApprovalsCard approvals={card.approvals} />
    case 'run':
      return <RunCard runId={card.run_id} workflow={card.workflow} />
    case 'table':
      return <TableCard title={card.title} columns={card.columns} rows={card.rows} {...(card.source ? { source: card.source } : {})} />
    case 'connector':
      return <ConnectorCard id={card.connector_id} why={card.why} />
  }
}
