import { useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { CleanupPlan, OgunError, StorageRules } from '../../api/types'
import { fmtAgo, fmtBytes, fmtCount, fmtTime, idempotencyKey } from '../../lib/format'
import { href, useAsync, useBackend } from '../../lib/hooks'
import { useScope } from '../../lib/scope'
import { Chip, Dialog, Empty, ErrorBox, Loading, Notice, PageHead, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { ScopeBar } from '../../ui/ScopeBar'

/** "Off" or a number of days, with a switch, as T3 Code's cleanup rules are. */
function DaysRule({ title, desc, days, onChange, fallback }: { title: string; desc: string; days: number; onChange: (d: number) => void; fallback: number }) {
  const on = days > 0
  return (
    <SettingRow title={title} desc={desc}>
      <span class="withunit">
        {on ? (
          <>
            <input type="number" aria-label={`${title}, days`} min={1} max={3650} value={days} onChange={(e) => onChange(Math.max(1, Math.round(Number((e.target as HTMLInputElement).value) || 1)))} />
            <span class="muted">days</span>
          </>
        ) : (
          <span class="muted">Off</span>
        )}
        <Switch label={title} checked={on} onChange={(v) => onChange(v ? fallback : 0)} />
      </span>
    </SettingRow>
  )
}

export function StorageSettings() {
  const backend = useBackend()
  const scope = useScope()
  const info = useAsync(() => (backend.storage ? backend.storage.info() : Promise.resolve(null)), [backend])
  const rules = useAsync(() => (backend.storage ? backend.storage.rules() : Promise.resolve(null)), [backend])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  const [plan, setPlan] = useState<CleanupPlan | null>(null)
  const [limit, setLimit] = useState<number | null>(null)
  const act = async (f: () => Promise<unknown>) => {
    setBusy(true)
    setError(undefined)
    try {
      await f()
      info.reload()
      rules.reload()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const set = (patch: Partial<StorageRules>) => act(() => backend.storage?.setRules(patch, idempotencyKey('rule')) ?? Promise.resolve())
  if (!backend.storage)
    return (
      <>
        <PageHead title="Storage" sub="The artifact store and its retention." />
        <Notice>
          This daemon does not report storage use or rules over this connection. Artifacts are content-addressed files under the daemon's state directory; <span class="mono">ogun artifacts</span> lists a run's.
        </Notice>
      </>
    )
  const s = info.data
  const r = rules.data
  const used = s ? s.artifacts.bytes + s.caches.bytes : 0
  const pct = s ? Math.min(100, (used / s.limit_bytes) * 100) : 0
  const byWorkflow = (s?.by_workflow ?? []).filter((w) => !scope.workflow || w.workflow === scope.workflow)
  return (
    <>
      <PageHead title="Storage" sub="The content-addressed artifact store: every file named by its BLAKE3 digest, written once." />
      <ScopeBar workflows environments={false} />
      <ErrorBox error={info.error} />
      <ErrorBox error={rules.error} />
      <ErrorBox error={error} title="Storage" />
      {info.loading && !s ? <Loading /> : null}
      {s && r ? (
        <>
          <SettingsGroup title="Use">
            <div class="row stack">
              <div class="text">
                <div class="title">
                  {fmtBytes(used)} <span class="muted" style={{ fontWeight: 400 }}>of {fmtBytes(s.limit_bytes)} budget</span>
                </div>
                <div class="desc">
                  {fmtCount(s.artifacts.count)} artifacts ({fmtBytes(s.artifacts.bytes)}) from {fmtCount(s.runs.count)} runs, and {fmtBytes(s.caches.bytes)} of derived caches.
                </div>
              </div>
              <div class="meter">
                <span style={{ width: `${pct.toFixed(1)}%` }} />
              </div>
            </div>
            <div class="table-wrap flush">
              <table>
                <thead>
                  <tr>
                    <th>Workflow</th>
                    <th class="num">Artifacts</th>
                    <th class="num">Size</th>
                  </tr>
                </thead>
                <tbody>
                  {byWorkflow.map((w) => (
                    <tr>
                      <td>
                        <a href={href('workflows', w.workflow)}>{w.workflow}</a>
                      </td>
                      <td class="num">{fmtCount(w.artifacts)}</td>
                      <td class="num">{fmtBytes(w.bytes)}</td>
                    </tr>
                  ))}
                  {byWorkflow.length === 0 ? (
                    <tr>
                      <td colSpan={3} class="muted">
                        Nothing stored for this workflow.
                      </td>
                    </tr>
                  ) : null}
                </tbody>
              </table>
            </div>
          </SettingsGroup>
          <SettingsGroup title="Location and budget">
            <SettingRow title="Artifact store" desc="Where the daemon keeps artifacts. Set in config.json; moving it is done on the daemon host, with the daemon stopped.">
              <span class="mono pathbox">{r.store_location}</span>
            </SettingRow>
            <SettingRow title="Storage budget" desc="The size the meter above measures against. It warns; it never deletes by itself.">
              <span class="withunit">
                <input type="number" aria-label="Storage budget, GiB" min={1} max={100_000} value={limit ?? r.limit_gib} onInput={(e) => setLimit(Number((e.target as HTMLInputElement).value))} onChange={() => limit && limit > 0 && void set({ limit_gib: Math.round(limit) }).then(() => setLimit(null))} />
                <span class="muted">GiB</span>
              </span>
            </SettingRow>
          </SettingsGroup>
          <SettingsGroup title="Retention" note="Each rule removes artifacts only if no later run's lineage still reads them, and never one a pending approval refers to. A removed artifact can be produced again by running its partition.">
            <DaysRule title="Delete finished runs' artifacts" desc="Counted from the run that produced them. Archived runs keep their hashes and lineage." days={r.retention_days} fallback={90} onChange={(d) => void set({ retention_days: d })} />
            <DaysRule title="Delete artifacts of failed runs" desc="A failed run's partial outputs. Its error and lineage are kept." days={r.delete_failed_after_days} fallback={30} onChange={(d) => void set({ delete_failed_after_days: d })} />
            <DaysRule title="Delete artifacts of cancelled runs" desc="Whatever a cancelled run had written before it stopped." days={r.delete_cancelled_after_days} fallback={7} onChange={(d) => void set({ delete_cancelled_after_days: d })} />
            <DaysRule title="Delete unfinished transfers" desc="Partial downloads the engine could no longer resume." days={r.delete_partials_after_days} fallback={2} onChange={(d) => void set({ delete_partials_after_days: d })} />
            <SettingRow
              title={
                <>
                  Keep what lineage still reads <Chip>fixed</Chip>
                </>
              }
              desc="An artifact that a later run took as input is never removed by a retention rule. This cannot be turned off."
            >
              <Switch label="Keep what lineage still reads" checked disabled onChange={() => undefined} />
            </SettingRow>
          </SettingsGroup>
          <SettingsGroup title="Caches and logs">
            <DaysRule title="Clear derived caches" desc="Parsed pages, decompressed archives and query results. They can always be rebuilt from artifacts." days={r.clear_caches_after_days} fallback={30} onChange={(d) => void set({ clear_caches_after_days: d })} />
            <DaysRule title="Delete old rotated logs" desc="Inactive rotated log files after this many days. The current log is kept." days={r.delete_logs_after_days} fallback={14} onChange={(d) => void set({ delete_logs_after_days: d })} />
          </SettingsGroup>
          <SettingsGroup title="Cleanup">
            <SettingRow title="Delete now" desc="Applies every rule above once, now. You see what it would remove first, and nothing is removed until you confirm.">
              <button type="button" class="btn" disabled={busy} onClick={() => act(async () => setPlan((await backend.storage?.cleanupPlan()) ?? null))}>
                <Icon name="trash" /> Review cleanup
              </button>
            </SettingRow>
          </SettingsGroup>
        </>
      ) : null}
      {plan ? (
        <Dialog title="Delete now?" onClose={() => setPlan(null)}>
          {plan.lines.length === 0 && plan.caches_bytes === 0 && plan.logs === 0 ? (
            <p>No rule is switched on, so there is nothing to remove.</p>
          ) : (
            <>
              <p>
                This removes <strong>{fmtCount(plan.artifacts)}</strong> artifacts ({fmtBytes(plan.bytes)}), {fmtBytes(plan.caches_bytes)} of caches and {fmtCount(plan.logs)} log files. Runs, hashes and lineage stay.
              </p>
              <div class="table-wrap flush">
                <table>
                  <tbody>
                    {plan.lines.map((l) => (
                      <tr>
                        <td class="wrap">{l.rule}</td>
                        <td class="num">{fmtCount(l.artifacts)}</td>
                        <td class="num">{fmtBytes(l.bytes)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          )}
          <div class="decide-row">
            <button type="button" class="btn" onClick={() => setPlan(null)}>
              Keep everything
            </button>
            <button
              type="button"
              class="btn danger"
              onClick={() => {
                setPlan(null)
                void act(() => backend.storage?.cleanup(idempotencyKey('cleanup')) ?? Promise.resolve())
              }}
            >
              Delete now
            </button>
          </div>
        </Dialog>
      ) : null}
    </>
  )
}

export function ArchiveSettings() {
  const backend = useBackend()
  const scope = useScope()
  const list = useAsync(() => (backend.archive ? backend.archive.list() : Promise.resolve(null)), [backend])
  const [error, setError] = useState<OgunError | undefined>()
  if (!backend.archive)
    return (
      <>
        <PageHead title="Archive" sub="Workflow versions and runs set aside." />
        <Notice>This daemon has no archive. Old workflow versions stay stored and addressable by their hash; runs stay in the run store.</Notice>
      </>
    )
  const restore = async (id: string) => {
    setError(undefined)
    try {
      await backend.archive?.restore(id, idempotencyKey('restore'))
      list.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  const items = (list.data?.items ?? []).filter((i) => !scope.workflow || i.name.startsWith(scope.workflow) || i.detail.startsWith(scope.workflow))
  return (
    <>
      <PageHead title="Archive" sub="Set aside, not deleted: restoring puts an item back with its hash and lineage unchanged. Auto-archiving is set under General." />
      <ScopeBar workflows environments={false} />
      <ErrorBox error={list.error} />
      <ErrorBox error={error} title="Restore" />
      {list.loading && !list.data ? <Loading /> : null}
      {list.data && items.length === 0 ? <Empty>Nothing is archived{scope.workflow ? ` for ${scope.workflow}` : ''}.</Empty> : null}
      {items.length > 0 ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Kind</th>
                <th>Name</th>
                <th>Detail</th>
                <th>Archived</th>
                <th>Reason</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {items.map((i) => (
                <tr>
                  <td class="mono">{i.kind.replace('_', ' ')}</td>
                  <td class="mono">{i.name}</td>
                  <td class="wrap">{i.detail}</td>
                  <td title={fmtTime(i.archived_at)}>{fmtAgo(i.archived_at)}</td>
                  <td class="wrap">{i.reason}</td>
                  <td>
                    <button type="button" class="btn small" onClick={() => void restore(i.id)}>
                      Restore
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </>
  )
}
