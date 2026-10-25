// The approvals inbox: one of Ogun's human trust surfaces (constraint 3, Zeus ADR 0007). It shows only facts the
// daemon computed, in a fixed order (what, scope, workflow version and hash, who asked), keeps words from agents
// in a separate labelled block, puts approve and deny at equal size, and decides only after an explicit click on
// a button that names the decision, once it has armed. No ornament, no animation, nothing automatic.

import { useEffect, useState } from 'preact/hooks'

import type { PreparedDecision } from '../api'
import { asOgunError } from '../api'
import type { ApprovalState, ApprovalView, Decision, OgunError } from '../api/types'
import { fmtAgo, fmtTime, words } from '../lib/format'
import { href, useAsync, useBackend, useInfo, useLive } from '../lib/hooks'
import { Dialog, Empty, ErrorBox, Hash, Loading, Notice, PageHead, StateLabel, Untrusted } from '../ui/components'

const STATES: ApprovalState[] = ['pending', 'approved', 'denied', 'expired']

export function Approvals({ state }: { state?: string }) {
  const backend = useBackend()
  const filter = (STATES as string[]).includes(state ?? '') ? (state as ApprovalState) : 'pending'
  const list = useAsync(() => backend.approvalsList(filter), [backend, filter])
  useLive(() => list.reload())
  return (
    <>
      <PageHead title="Approvals" />
      <div class="toolbar">
        {STATES.map((s) => (
          <a class={`btn small ${s === filter ? 'primary' : ''}`} href={href('approvals', { state: s })}>
            {s}
          </a>
        ))}
      </div>
      <ErrorBox error={list.error} />
      {list.loading && !list.data ? <Loading /> : null}
      {list.data && list.data.approvals.length === 0 ? <Empty>No {filter} approvals.</Empty> : null}
      {list.data && list.data.approvals.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>What</th>
                <th>Workflow</th>
                <th>Requested by</th>
                <th>State</th>
                <th>Requested</th>
                <th class="hide-narrow">Expires</th>
              </tr>
            </thead>
            <tbody>
              {list.data.approvals.map((a) => (
                <tr>
                  <td>
                    <a href={href('approvals', a.approval_id)}>{words(a.kind)}</a>
                    {a.node ? <span class="muted"> · {a.node}</span> : null}
                  </td>
                  <td>
                    {a.workflow} <Hash value={a.workflow_hash} short />
                  </td>
                  <td class="mono">{a.requested_by}</td>
                  <td>
                    <StateLabel state={a.state} />
                  </td>
                  <td>{fmtAgo(a.created_at)}</td>
                  <td class="hide-narrow">{fmtAgo(a.expires_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </>
  )
}

/** The facts, always in the same order and the same form. */
function Facts({ a }: { a: ApprovalView }) {
  const rows: [string, string][] = [
    ['what', words(a.kind)],
    ...a.facts.map((f) => [f.label, f.value] as [string, string]),
    ['workflow', a.workflow],
    ['workflow hash', a.workflow_hash],
    ...(a.run_id ? [['run', a.run_id] as [string, string]] : []),
    ...(a.node ? [['node', a.node] as [string, string]] : []),
    ['requested by', a.requested_by],
    ['requested at', fmtTime(a.created_at)],
    ['expires at', fmtTime(a.expires_at)],
    ['request hash', a.request_hash],
  ]
  return (
    <div class="facts">
      {rows.map(([k, v]) => (
        <div class="row">
          <div>{k}</div>
          <div>{v}</div>
        </div>
      ))}
    </div>
  )
}

function whereToDecide(mobile: boolean, link: string): string {
  if (mobile) {
    return 'Approvals on Android and iOS come later, with biometric channel evidence (O-12). Decide in Ogun Studio on the desktop, in the ogun CLI on the daemon host, or in Studio-web with a passkey.'
  }
  if (link === 'hub') {
    return 'This is a read-only hub link: an API token is never a person. Decide in Studio-web on the server with a passkey, or on the hub host.'
  }
  if (link === 'server') {
    return 'A person decides this at the terminal of the server host. Deciding in Studio-web needs remote approvals switched on for this server (off until the security review of ADR 0010), the approver role and a confirmed passkey.'
  }
  return 'This connection cannot decide approvals. The studio credential must be a human credential, and the daemon accepts Studio evidence only from the pinned ogun-studio binary.'
}

export function ApprovalDetail({ approvalId }: { approvalId: string }) {
  const backend = useBackend()
  const { info } = useInfo()
  const approval = useAsync(() => backend.approvalsGet(approvalId), [backend, approvalId])
  useLive(() => approval.reload())
  const [review, setReview] = useState<PreparedDecision | null>(null)
  const [error, setError] = useState<OgunError | undefined>()
  const [done, setDone] = useState<ApprovalView | null>(null)
  const [busy, setBusy] = useState(false)
  const a = done ?? approval.data
  const canDecide = !!info?.decideChannel && a?.state === 'pending'

  const begin = async (d: Decision) => {
    setError(undefined)
    setBusy(true)
    try {
      setReview(await backend.prepareDecision(approvalId, d))
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const close = () => {
    review?.cancel()
    setReview(null)
  }
  const confirm = async () => {
    if (!review) return
    setBusy(true)
    try {
      const v = await review.confirm()
      setDone(v)
      setReview(null)
      approval.reload()
    } catch (e) {
      setError(asOgunError(e))
      setReview(null)
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <PageHead title="Approval">
        <a class="btn" href={href('approvals')}>
          Inbox
        </a>
      </PageHead>
      <ErrorBox error={approval.error} />
      {approval.loading && !a ? <Loading /> : null}
      {a ? (
        <>
          <p>
            <StateLabel state={a.state} />
            {a.decided_at ? (
              <span class="muted">
                {' '}
                · decided {fmtTime(a.decided_at)} via {a.decided_via ?? '–'} by {a.decided_by ?? '–'}
              </span>
            ) : null}
          </p>
          <Facts a={a} />
          <Untrusted label="Reason given by the requester" text={a.untrusted_reason} />
          {a.run_id ? (
            <p style={{ marginTop: '12px' }}>
              <a href={href('runs', a.run_id)}>Open the run</a>
            </p>
          ) : null}
          <ErrorBox error={error} title="Decision" />
          {canDecide ? (
            <div class="decide-row">
              <button type="button" class="btn" disabled={busy} onClick={() => begin('approve')}>
                Approve…
              </button>
              <button type="button" class="btn" disabled={busy} onClick={() => begin('deny')}>
                Deny…
              </button>
            </div>
          ) : a.state === 'pending' && info ? (
            <Notice kind="warn">{whereToDecide(info.mobile, info.link)}</Notice>
          ) : null}
        </>
      ) : null}
      {review ? <ConfirmDecision review={review} busy={busy} onClose={close} onConfirm={confirm} channel={info?.decideChannel ?? null} /> : null}
    </>
  )
}

function ConfirmDecision({
  review,
  busy,
  onClose,
  onConfirm,
  channel,
}: {
  review: PreparedDecision
  busy: boolean
  onClose: () => void
  onConfirm: () => void
  channel: 'studio' | 'webauthn' | null
}) {
  const [armed, setArmed] = useState(false)
  useEffect(() => {
    const t = setTimeout(() => setArmed(true), review.armMs)
    return () => clearTimeout(t)
  }, [review])
  const a = review.approval
  const verb = review.decision === 'approve' ? 'Approve' : 'Deny'
  const label = `${verb} ${words(a.kind)} for ${a.workflow}`
  return (
    <Dialog title={`${verb} this request?`} onClose={onClose}>
      <p>
        You are about to <strong>{review.decision}</strong> the request below. The decision binds to request hash{' '}
        <Hash value={a.request_hash} short />; if the daemon's facts change, it is refused.
        {channel === 'webauthn' ? ' Your passkey will ask you to confirm.' : ''}
      </p>
      <Facts a={a} />
      <Untrusted label="Reason given by the requester" text={a.untrusted_reason} />
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose} disabled={busy}>
          Back
        </button>
        <button type="button" class="btn" onClick={onConfirm} disabled={!armed || busy} aria-describedby="arming">
          {label}
        </button>
      </div>
      <p id="arming" class="muted" style={{ marginTop: '8px', fontSize: '13px' }}>
        {armed ? 'Armed.' : 'The button arms in a moment.'}
      </p>
    </Dialog>
  )
}
