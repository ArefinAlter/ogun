import type { EnvironmentView } from '../api/types'
import { useAsync, useBackend, useInfo } from '../lib/hooks'
import { setScope, useScope } from '../lib/scope'

/** "Applying settings for [All workflows] across [All environments]". */
export function ScopeBar({ workflows = true, environments = true }: { workflows?: boolean; environments?: boolean }) {
  const backend = useBackend()
  const { info } = useInfo()
  const scope = useScope()
  const list = useAsync(() => backend.workflowsList().catch(() => ({ workflows: [] })), [backend])
  const envs = useAsync(
    () =>
      backend.environments
        ? backend.environments().then((r) => r.environments)
        : Promise.resolve<EnvironmentView[]>([{ id: 'local', name: info?.link === 'hub' ? 'This hub' : 'This daemon', kind: info?.link === 'hub' ? 'hub' : 'local', connected: !!info?.connected }]),
    [backend, info?.link],
  )
  return (
    <div class="scopebar">
      <span class="muted">Applying settings</span>
      {workflows ? (
        <>
          <span class="muted">for</span>
          <select class="scope-select" aria-label="Workflow" value={scope.workflow} onChange={(e) => setScope({ workflow: (e.target as HTMLSelectElement).value })}>
            <option value="">All workflows</option>
            {list.data?.workflows.map((w) => (
              <option value={w.name}>{w.name}</option>
            ))}
          </select>
        </>
      ) : null}
      {environments ? (
        <>
          <span class="muted">{workflows ? 'across' : 'on'}</span>
          <select class="scope-select" aria-label="Environment" value={scope.environment} onChange={(e) => setScope({ environment: (e.target as HTMLSelectElement).value })}>
            <option value="">All environments</option>
            {envs.data?.map((e) => (
              <option value={e.id}>{e.name}</option>
            ))}
          </select>
        </>
      ) : null}
    </div>
  )
}
