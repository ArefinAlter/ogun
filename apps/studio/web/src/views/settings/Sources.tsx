import { useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { OgunError, SourceProvider } from '../../api/types'
import { fmtAgo, fmtTime, idempotencyKey } from '../../lib/format'
import { useAsync, useBackend } from '../../lib/hooks'
import { Chip, CodeCopy, ErrorBox, Loading, Notice, PageHead, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { ScopeBar } from '../../ui/ScopeBar'

function stateChip(p: SourceProvider) {
  if (p.state === 'available') return <Chip tone="ok">available</Chip>
  if (p.state === 'not_authenticated') return <Chip tone="warn">not authenticated</Chip>
  return <Chip>not available</Chip>
}

export function SourcesSettings() {
  const backend = useBackend()
  const src = useAsync(() => (backend.sources ? backend.sources.get() : Promise.resolve(null)), [backend])
  const [error, setError] = useState<OgunError | undefined>()
  const [syncing, setSyncing] = useState<string | null>(null)
  const act = async (f: () => Promise<unknown>) => {
    setError(undefined)
    try {
      await f()
      src.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  if (!backend.sources)
    return (
      <>
        <PageHead title="Workflow sources" sub="Where workflow documents come from." />
        <Notice>
          This daemon does not watch folders or repositories. A person stores a workflow version with the CLI, which also bundles the code files its <span class="mono">code.path</span> members name:
        </Notice>
        <CodeCopy text="ogun put path/to/name.workflow.json" />
        <Notice>
          A version that references new code, new egress or a budget over policy waits for an approval, however it was stored.
        </Notice>
      </>
    )
  const v = src.data
  return (
    <>
      <PageHead title="Workflow sources" sub="Folders and repositories whose *.workflow.json files are stored as new versions." />
      <ScopeBar workflows={false} environments />
      <ErrorBox error={src.error} />
      <ErrorBox error={error} title="Sources" />
      {src.loading && !v ? <Loading /> : null}
      {v ? (
        <>
          <SettingsGroup title="Repositories" note="Storing a version never runs it. A version that needs code, egress or budget approvals waits in waiting_for_user until a person decides, whatever the source.">
            <SettingRow title="Automatically pull" desc="Keeps checked-out repositories current when the checkout has no local changes or commits.">
              <Switch label="Automatically pull" checked={v.auto_pull} onChange={(on) => void act(() => backend.sources?.setAutoPull(on, idempotencyKey('pull')) ?? Promise.resolve())} />
            </SettingRow>
            {v.repos.map((r) => (
              <div class="row stack" key={r.id}>
                <div class="repo-head">
                  <span class="well">
                    <Icon name={r.kind === 'git' ? 'dag' : 'drawers'} />
                  </span>
                  <div class="text">
                    <div class="title">
                      {r.name} <Chip>{r.kind}</Chip>
                    </div>
                    <div class="desc mono">
                      {r.location}
                      {r.branch ? ` · ${r.branch}` : ''}
                    </div>
                  </div>
                  <button type="button" class="btn small" disabled={syncing === r.id} onClick={() => void act(async () => { setSyncing(r.id); await backend.sources?.sync(r.id, idempotencyKey('sync')); setSyncing(null) })}>
                    {syncing === r.id ? 'Syncing…' : 'Sync now'}
                  </button>
                </div>
                <div class="repo-body">
                  <label class="check">
                    <Switch label={`Watch ${r.name}`} checked={r.watch} onChange={(on) => void act(() => backend.sources?.setRepo(r.id, { watch: on }, idempotencyKey('watch')) ?? Promise.resolve())} />
                    Watch for changes
                  </label>
                  <label class="field inline">
                    On a change
                    <select aria-label={`On a change in ${r.name}`} value={r.import_mode} onChange={(e) => void act(() => backend.sources?.setRepo(r.id, { import_mode: (e.target as HTMLSelectElement).value as 'store' | 'store_validate' }, idempotencyKey('mode')) ?? Promise.resolve())}>
                      <option value="store">Store the version</option>
                      <option value="store_validate">Validate, then store</option>
                    </select>
                  </label>
                  <span class="muted" title={fmtTime(r.last_sync_at)}>
                    {r.workflows} workflows · {r.last_sync_at ? `last sync ${fmtAgo(r.last_sync_at)}` : 'never synced'}
                    {r.last_result ? ` · ${r.last_result}` : ''}
                  </span>
                </div>
              </div>
            ))}
          </SettingsGroup>
          <SettingsGroup title="Source control providers" note="Providers add pull-request and release links to a workflow's page. None of them can decide an approval.">
            {v.providers.map((p) => (
              <div class="row" key={p.id}>
                <div class="text">
                  <div class="title">
                    {p.name} {stateChip(p)} {p.region === 'cn' ? <Chip>China</Chip> : null}
                  </div>
                  <div class="desc">{p.detail}</div>
                </div>
                <div class="control">
                  <Switch label={p.name} checked={p.enabled} disabled={p.state !== 'available'} onChange={(on) => void act(() => backend.sources?.setProvider(p.id, on, idempotencyKey('prov')) ?? Promise.resolve())} />
                </div>
              </div>
            ))}
          </SettingsGroup>
        </>
      ) : null}
    </>
  )
}
