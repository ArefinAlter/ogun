import { useMemo, useState } from 'preact/hooks'

import type { CatalogFilter, ConnectorManifest, Maturity } from '../../connectors'
import { CATEGORIES, DIRECTIONS, MATURITY, REGION_LABEL, categoryLabel, expandDomains, matchesFilter, nodeSnippet, secretCommand, useCatalog } from '../../connectors'
import type { Category } from '../../connectors'
import { href, useAsync, useBackend, useInfo } from '../../lib/hooks'
import { Chip, CodeCopy, Empty, ExternalLink, Loading, Notice, PageHead, Segmented, SettingsGroup, Tabs } from '../../ui/components'
import type { IconName } from '../../ui/icons'
import { Icon } from '../../ui/icons'

const CATEGORY_ICON: Record<Category, IconName> = {
  warehouse: 'database',
  database: 'database2',
  object_storage: 'bucket',
  streaming: 'wave',
  orchestration: 'dag',
  observability: 'gauge',
  catalog: 'ledger',
  quality: 'seal',
  notification: 'bell',
  source: 'globe',
  edge: 'compass',
  agent: 'code',
}

const MATURITY_TONE: Record<Maturity, '' | 'ok' | 'accent'> = { contract: '', built: 'ok', verified: 'accent' }

function MaturityChip({ m }: { m: ConnectorManifest }) {
  return (
    <span title={MATURITY[m.maturity].long}>
      <Chip tone={MATURITY_TONE[m.maturity]}>{MATURITY[m.maturity].label}</Chip>
    </span>
  )
}

function Card({ m, connections }: { m: ConnectorManifest; connections: number }) {
  return (
    <a class="ccard" href={href('settings', 'connectors', m.id)}>
      <div class="ccard-head">
        <span class="well">
          <Icon name={CATEGORY_ICON[m.category]} />
        </span>
        <div class="ccard-title">
          <div class="name">{m.name}</div>
          <div class="vendor">{m.vendor}</div>
        </div>
      </div>
      <p class="ccard-summary">{m.summary}</p>
      <div class="ccard-foot">
        <MaturityChip m={m} />
        {m.regions.includes('cn') ? <Chip>{m.regions.includes('global') ? 'Global and China' : 'China'}</Chip> : null}
        {connections > 0 ? <Chip tone="info">{connections} connection{connections > 1 ? 's' : ''}</Chip> : null}
      </div>
    </a>
  )
}

export function ConnectorsCatalog() {
  const catalog = useCatalog()
  const backend = useBackend()
  const conns = useAsync(() => (backend.connections ? backend.connections.list() : Promise.resolve(null)), [backend])
  const [filter, setFilter] = useState<CatalogFilter>({ query: '', category: '', region: '', maturity: '' })
  const set = (patch: Partial<CatalogFilter>) => setFilter((f) => ({ ...f, ...patch }))
  const countBy = useMemo(() => {
    const m = new Map<string, number>()
    for (const c of conns.data?.connections ?? []) m.set(c.connector, (m.get(c.connector) ?? 0) + 1)
    return m
  }, [conns.data])
  const withoutCategory = useMemo(() => (catalog ?? []).filter((m) => matchesFilter(m, { ...filter, category: '' })), [catalog, filter])
  const shown = useMemo(() => (filter.category ? withoutCategory.filter((m) => m.category === filter.category) : withoutCategory), [withoutCategory, filter.category])
  const operators = (catalog ?? []).reduce((n, m) => n + m.operators.length, 0)
  return (
    <>
      <PageHead title="Connectors" sub={catalog ? `${catalog.length} systems Ogun can read from, write to, emit to, or be called by; ${operators} operators between them.` : 'Loading the catalog'} />
      <Notice>
        The catalog is a set of manifests: what each system is, where Ogun would connect, what to approve, and which operators it would contribute. A manifest holds no secret. <strong>Manifest only</strong> means no operator exists yet and nothing has been run against that system; <strong>Built</strong> means the code exists and has its own tests.
      </Notice>
      <div class="toolbar wrap">
        <input type="search" aria-label="Search connectors" placeholder="Search by name, vendor, operator or host" size={40} value={filter.query} onInput={(e) => set({ query: (e.target as HTMLInputElement).value })} />
        <Segmented
          label="Region"
          value={filter.region || 'all'}
          onChange={(v) => set({ region: v === 'all' ? '' : v })}
          options={[
            ['all', 'All'],
            ['global', REGION_LABEL.global],
            ['cn', REGION_LABEL.cn],
          ]}
        />
        <select aria-label="Maturity" value={filter.maturity} onChange={(e) => set({ maturity: (e.target as HTMLSelectElement).value as Maturity | '' })}>
          <option value="">Any maturity</option>
          <option value="built">Built</option>
          <option value="contract">Manifest only</option>
          <option value="verified">Verified</option>
        </select>
      </div>
      <div class="chips" role="tablist" aria-label="Category">
        <button type="button" role="tab" aria-selected={filter.category === ''} class={`catchip ${filter.category === '' ? 'on' : ''}`} onClick={() => set({ category: '' })}>
          All <span class="n">{withoutCategory.length}</span>
        </button>
        {CATEGORIES.map((c) => {
          const n = withoutCategory.filter((m) => m.category === c.id).length
          if (!n && filter.category !== c.id) return null
          return (
            <button type="button" role="tab" aria-selected={filter.category === c.id} class={`catchip ${filter.category === c.id ? 'on' : ''}`} onClick={() => set({ category: c.id })} title={c.blurb}>
              {c.label} <span class="n">{n}</span>
            </button>
          )
        })}
      </div>
      {!catalog ? <Loading what="loading the catalog" /> : null}
      {catalog && shown.length === 0 ? <Empty>No connector matches. Try fewer words or another region.</Empty> : null}
      <div class="cat-grid">
        {shown.map((m) => (
          <Card m={m} connections={countBy.get(m.id) ?? 0} key={m.id} />
        ))}
      </div>
    </>
  )
}

function SecretFields({ m }: { m: ConnectorManifest }) {
  const rows = m.auth.flatMap((a) => a.fields.filter((f) => f.secret).map((f) => ({ method: a.label, field: f })))
  if (!rows.length) return <p class="muted">No secret is needed for the methods this connector supports.</p>
  return (
    <div class="table-wrap flush">
      <table>
        <thead>
          <tr>
            <th>Method</th>
            <th>Field</th>
            <th>Required</th>
            <th>Held as</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(({ method, field }) => (
            <tr>
              <td>{method}</td>
              <td>
                {field.label} <span class="mono muted">{field.name}</span>
              </td>
              <td>{field.required ? 'yes' : 'no'}</td>
              <td>
                <Chip tone="warn">secret</Chip> <span class="muted">a name in the secret store</span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

export function ConnectorDetail({ id }: { id: string }) {
  const catalog = useCatalog()
  const backend = useBackend()
  const { info } = useInfo()
  const [tab, setTab] = useState<'overview' | 'example'>('overview')
  const [opIndex, setOpIndex] = useState(0)
  const m = catalog?.find((x) => x.id === id)
  const desktop = info?.backend === 'tauri'
  if (!catalog) return <Loading what="loading the catalog" />
  if (!m)
    return (
      <>
        <PageHead title="Unknown connector" />
        <Empty>
          No connector is named <span class="mono">{id}</span>. <a href={href('settings', 'connectors')}>Back to the catalog</a>
        </Empty>
      </>
    )
  const op = m.operators[opIndex] ?? m.operators[0]
  const domains = expandDomains(m, {})
  const secretNames = m.auth.flatMap((a) => a.fields.filter((f) => f.secret && f.required).map((f) => f.name))
  return (
    <>
      <PageHead title={m.name} sub={`${m.vendor} · ${categoryLabel(m.category)}`}>
        <a class="btn" href={href('settings', 'connectors')}>
          All connectors
        </a>
        {backend.connections ? (
          <a class="btn primary" href={href('settings', 'connections', 'new', { connector: m.id })}>
            New connection
          </a>
        ) : null}
      </PageHead>
      <div class={`notice ${m.maturity === 'contract' ? '' : 'ok'}`}>
        <strong>{MATURITY[m.maturity].label}.</strong> {MATURITY[m.maturity].long}
        {m.implementation ? (
          <div class="evidence">
            Code: <span class="mono">{m.implementation.path}</span>
            {m.implementation.evidence ? <> · {m.implementation.evidence}</> : null}
          </div>
        ) : null}
      </div>
      <Tabs
        tabs={[
          ['overview', 'Overview'],
          ['example', 'In a workflow'],
        ]}
        value={tab}
        onChange={setTab}
      />
      {tab === 'overview' ? (
        <>
          <SettingsGroup title="About">
            <div class="row">
              <div class="text">
                <div class="title">{m.summary}</div>
                <div class="desc">
                  Documentation: <ExternalLink href={m.docs_url} desktop={desktop} />
                </div>
              </div>
            </div>
            <div class="row">
              <div class="text">
                <div class="title">What Ogun does with it</div>
                <div class="chips tight">
                  {m.directions.map((d) => (
                    <Chip>{DIRECTIONS[d]}</Chip>
                  ))}
                </div>
              </div>
              <div class="control">
                <span class="muted">Protocol</span> <span class="mono">{m.protocol}</span> · <span class="muted">Regions</span> {m.regions.map((r) => REGION_LABEL[r]).join(' and ')}
              </div>
            </div>
          </SettingsGroup>
          <SettingsGroup title="Operators" note={m.operators.length ? 'Operators run as sandboxed child processes; none holds a secret in its configuration, only a secret_ref.' : undefined}>
            {m.operators.length === 0 ? (
              <div class="row">
                <div class="text">
                  <div class="desc">This connector contributes no operator of its own.{m.notes ? ' See the notes below for how it is used.' : ''}</div>
                </div>
              </div>
            ) : (
              <div class="table-wrap flush">
                <table>
                  <thead>
                    <tr>
                      <th>Operator</th>
                      <th>What it does</th>
                      <th>Reaches it by</th>
                      <th>Outputs</th>
                    </tr>
                  </thead>
                  <tbody>
                    {m.operators.map((o) => (
                      <tr>
                        <td class="mono">{o.op}</td>
                        <td class="wrap">{o.summary}</td>
                        <td class="mono">{o.via ?? '–'}{o.tool ? ` (${o.tool})` : ''}</td>
                        <td class="mono">{o.outputs.map((x) => x.name).join(', ') || '–'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </SettingsGroup>
          <SettingsGroup title="Network" note={m.egress.note}>
            {domains.length === 0 ? (
              <div class="row">
                <div class="text">
                  <div class="desc">Ogun reaches nothing outside the machine for this connector.</div>
                </div>
              </div>
            ) : (
              <div class="row stack">
                <div class="text">
                  <div class="title">Hosts to allow in an egress approval</div>
                  <div class="desc">Parts in braces are filled from the connection's settings.</div>
                </div>
                <ul class="mono namelist">
                  {domains.map((d) => (
                    <li>{d.host}</li>
                  ))}
                </ul>
                <div class="muted">Ports {m.egress.ports.join(', ')}</div>
              </div>
            )}
            {m.approvals?.length ? (
              <div class="row">
                <div class="text">
                  <div class="title">Needs a person's approval</div>
                  <div class="desc">A workflow version that uses this connector waits in waiting_for_user until these are decided.</div>
                </div>
                <div class="control chips tight">
                  {m.approvals.map((a) => (
                    <Chip tone="warn">{a.replace('_', ' ')}</Chip>
                  ))}
                </div>
              </div>
            ) : null}
          </SettingsGroup>
          <SettingsGroup title="Authentication and settings">
            <div class="row stack">
              <div class="text">
                <div class="title">Secrets</div>
                <div class="desc">
                  Each is the name of an entry in Ogun's secret store. Set a value on a terminal with <span class="mono">ogun secrets set &lt;name&gt;</span>; Studio never asks for one.
                </div>
              </div>
              <SecretFields m={m} />
            </div>
            {m.settings.length ? (
              <div class="row stack">
                <div class="text">
                  <div class="title">Settings</div>
                </div>
                <div class="table-wrap flush">
                  <table>
                    <thead>
                      <tr>
                        <th>Setting</th>
                        <th>Type</th>
                        <th>Default</th>
                        <th>Notes</th>
                      </tr>
                    </thead>
                    <tbody>
                      {m.settings.map((s) => (
                        <tr>
                          <td>
                            {s.label} <span class="mono muted">{s.name}</span>
                            {s.required ? <span class="req"> required</span> : null}
                          </td>
                          <td class="mono">{s.type === 'enum' ? s.options?.join(' | ') : s.type}</td>
                          <td class="mono">{s.default === undefined ? '–' : String(s.default)}</td>
                          <td class="wrap">{s.help ?? ''}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            ) : null}
          </SettingsGroup>
          {m.notes ? (
            <SettingsGroup title="Notes">
              <div class="row">
                <div class="text">
                  <div class="desc" style={{ color: 'var(--foreground)' }}>{m.notes}</div>
                </div>
              </div>
            </SettingsGroup>
          ) : null}
        </>
      ) : (
        <>
          {op ? (
            <SettingsGroup title="A node that uses it" note={m.maturity === 'contract' ? `Illustrative. ${op.op} is not in Ogun's operator catalogue yet, so ogun validate refuses a workflow that names it.` : 'Secrets are secret_ref members; no value ever appears in a workflow.'}>
              {m.operators.length > 1 ? (
                <div class="row">
                  <div class="text">
                    <div class="title">Operator</div>
                  </div>
                  <div class="control">
                    <select aria-label="Operator" value={String(opIndex)} onChange={(e) => setOpIndex(Number((e.target as HTMLSelectElement).value))}>
                      {m.operators.map((o, i) => (
                        <option value={String(i)}>{o.op}</option>
                      ))}
                    </select>
                  </div>
                </div>
              ) : null}
              <div class="row stack">
                <CodeCopy text={nodeSnippet(m, op)} label="Copy node" />
              </div>
            </SettingsGroup>
          ) : (
            <Notice>This connector has no operator of its own. {m.notes ?? ''}</Notice>
          )}
          <SettingsGroup title="Before the first run">
            {secretNames.length ? (
              secretNames.map((n) => (
                <div class="row stack">
                  <div class="text">
                    <div class="title">Store {n}</div>
                    <div class="desc">On the machine that runs the daemon. The value is typed without echo or piped in.</div>
                  </div>
                  <CodeCopy text={secretCommand(`${m.id.replace(/-/g, '_')}_${n}`)} label="Copy command" />
                </div>
              ))
            ) : (
              <div class="row">
                <div class="text">
                  <div class="desc">No secret is required.</div>
                </div>
              </div>
            )}
            <div class="row">
              <div class="text">
                <div class="title">Approve the hosts</div>
                <div class="desc">The first run of a version that names this connector asks for an egress approval listing the hosts above.</div>
              </div>
            </div>
          </SettingsGroup>
        </>
      )}
    </>
  )
}
