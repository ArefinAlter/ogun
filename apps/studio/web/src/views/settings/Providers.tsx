import { useEffect, useMemo, useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { LadderProvider, OgunError } from '../../api/types'
import { PROVIDERS, providerHost } from '../../connectors/providers'
import type { ProviderPreset } from '../../connectors/providers'
import { secretCommand } from '../../connectors'
import { download } from '../../lib/evidence'
import { useAgentSettings } from '../../lib/agentHooks'
import { fmtCount, idempotencyKey } from '../../lib/format'
import { useAsync, useBackend, useInfo } from '../../lib/hooks'
import { DEFAULT_BASE, KINDS, NEEDS_KEY, defaultCapabilities, isLoopback, ladderDocument, readLadder, suggestSecretName, toMicro, usd, validateLadder } from '../../lib/ladder'
import { Chip, CodeCopy, CopyButton, ErrorBox, ExternalLink, Loading, Notice, PageHead, Segmented, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'

function fromPreset(p: ProviderPreset, taken: Set<string>): LadderProvider {
  let name = p.entry_name
  let n = 2
  while (taken.has(name)) name = `${p.entry_name}_${n++}`
  return {
    name,
    kind: p.kind,
    enabled: false,
    base_url: p.base_url,
    model: '',
    ...(p.secret ? { api_key_secret: p.secret } : {}),
    input_per_mtok_usd_micro: 0,
    output_per_mtok_usd_micro: 0,
    max_output_tokens: 8192,
    timeout_ms: 120_000,
    capabilities: defaultCapabilities(p.kind),
    allow_insecure_http: false,
  }
}

function providerHostOf(url: string): string {
  try {
    return new URL(url).host || url
  } catch {
    return url
  }
}

function priceUnset(p: LadderProvider): boolean {
  return p.enabled && p.kind !== 'ollama' && !isLoopbackUrl(p.base_url) && p.input_per_mtok_usd_micro === 0 && p.output_per_mtok_usd_micro === 0
}

function isLoopbackUrl(u: string): boolean {
  try {
    return isLoopback(new URL(u).hostname)
  } catch {
    return false
  }
}

/** A masked field for one provider's key. The value lives in this input until Save; it is then sent once to the
 *  engine's secret store and cleared. It is never put in a URL, local storage or any other state. */
function KeyEntry({ required, stored, onSave, onRemove }: { required: boolean; stored: boolean; onSave: (value: string) => Promise<void>; onRemove: () => Promise<void> }) {
  const [value, setValue] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  const save = async () => {
    setBusy(true)
    setError(undefined)
    const v = value
    setValue('')
    try {
      await onSave(v)
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <div class="stack-tight">
      <div class="label">
        API key {required ? <span class="req">required to enable</span> : <span class="muted">optional</span>} {stored ? <Chip tone="ok">key stored</Chip> : <Chip tone={required ? 'bad' : ''}>no key stored</Chip>}
      </div>
      <div class="keyrow">
        <input
          type="password"
          autocomplete="off"
          spellcheck={false}
          aria-label="API key"
          placeholder={stored ? 'Paste a new key to replace the stored one' : 'Paste the key'}
          value={value}
          onInput={(e) => setValue((e.target as HTMLInputElement).value)}
        />
        <button type="button" class="btn small primary" disabled={busy || value.trim().length < 8} onClick={() => void save()}>
          Save key
        </button>
        {stored ? (
          <button type="button" class="btn small ghost" disabled={busy} onClick={() => void onRemove().catch((e) => setError(asOgunError(e)))}>
            Remove key
          </button>
        ) : null}
      </div>
      <div class="muted small-text">The key is kept in your operating system's credential store by the engine on this computer. It is never shown again, written to a log, or put in a prompt; the provider receives it only in the request header.</div>
      <ErrorBox error={error} title="Key" />
    </div>
  )
}

export function ProvidersSettings() {
  const backend = useBackend()
  const { info } = useInfo()
  const agentSettings = useAgentSettings()
  const ladder = useAsync(() => backend.llmLadder(), [backend])
  const secrets = useAsync(() => backend.secretsList().catch(() => ({ names: [] as string[] })), [backend])
  const [list, setList] = useState<LadderProvider[] | null>(null)
  const [sel, setSel] = useState(0)
  const [dirty, setDirty] = useState(false)
  const [region, setRegion] = useState<'all' | 'global' | 'cn'>('all')
  const [adding, setAdding] = useState(false)
  const [error, setError] = useState<OgunError | undefined>()
  const [applied, setApplied] = useState(false)
  const desktop = info?.backend === 'tauri'
  useEffect(() => {
    if (!dirty && ladder.data?.available) setList(readLadder(ladder.data.ladder) ?? [])
  }, [ladder.data, dirty])
  const edit = (i: number, patch: Partial<LadderProvider>) => {
    setDirty(true)
    setApplied(false)
    setList((l) => (l ? l.map((p, j) => (j === i ? { ...p, ...patch } : p)) : l))
  }
  const move = (i: number, d: -1 | 1) => {
    setDirty(true)
    setApplied(false)
    setList((l) => {
      if (!l || i + d < 0 || i + d >= l.length) return l
      const n = [...l]
      const [x] = n.splice(i, 1)
      n.splice(i + d, 0, x as LadderProvider)
      return n
    })
    setSel(i + d)
  }
  const remove = (i: number) => {
    setDirty(true)
    setApplied(false)
    setList((l) => (l ? l.filter((_, j) => j !== i) : l))
    setSel(Math.max(0, i - 1))
  }
  const add = (p: LadderProvider) => {
    const index = (list ?? []).length
    setDirty(true)
    setApplied(false)
    setList((l) => [...(l ?? []), p])
    setSel(index)
    setAdding(false)
  }
  const issues = useMemo(() => (list ? validateLadder(list) : []), [list])
  const doc = useMemo(() => (list ? ladderDocument(list) : ''), [list])
  const have = new Set(secrets.data?.names ?? [])
  const spend = ladder.data?.spend
  const cur = list?.[sel]
  const taken = new Set((list ?? []).map((p) => p.name))
  const presets = PROVIDERS.filter((p) => region === 'all' || p.regions.includes(region))
  const own = !!backend.engine
  const showPresets = adding || (own && ladder.data?.available === true && (list ?? []).length === 0)
  const as = agentSettings.data
  const setAgent = async (patch: Partial<NonNullable<typeof as>>) => {
    setError(undefined)
    try {
      await backend.agent?.setSettings(patch, idempotencyKey('agent-settings'))
      agentSettings.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  const saveKey = async (value: string) => {
    if (!cur || !backend.engine) return
    const name = cur.api_key_secret || suggestSecretName(cur.name)
    await backend.engine.putSecret(name, value)
    const inSync = !dirty
    if (cur.api_key_secret !== name) edit(sel, { api_key_secret: name })
    secrets.reload()
    // Name the key in the engine's ladder too, when that does not carry along other unsaved edits.
    if (inSync && list && cur.api_key_secret !== name) {
      const next = list.map((p, j) => (j === sel ? { ...p, api_key_secret: name } : p))
      if (validateLadder(next).length === 0) {
        await backend.setLadder?.(next, idempotencyKey('ladder'))
        setDirty(false)
        setApplied(true)
        ladder.reload()
      }
    }
  }
  const removeKey = async () => {
    if (!cur?.api_key_secret || !backend.engine) return
    await backend.engine.deleteSecret(cur.api_key_secret)
    secrets.reload()
  }
  const consented = as?.consented_providers ?? []
  const toggleConsent = (name: string, on: boolean) => void setAgent({ consented_providers: on ? [...new Set([...consented, name])] : consented.filter((n) => n !== name) })
  const apply = async () => {
    setError(undefined)
    try {
      await backend.setLadder?.(list ?? [], idempotencyKey('ladder'))
      setDirty(false)
      setApplied(true)
      ladder.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  return (
    <>
      <PageHead title="Providers" sub="Language and embedding models, tried in order, with a spend ledger. Optional: a workflow without an llm operator never touches a model." />
      <ErrorBox error={ladder.error} />
      <ErrorBox error={error} title="Ladder" />
      {ladder.loading && !ladder.data ? <Loading /> : null}
      {ladder.data && !ladder.data.available ? <Notice>This daemon does not report its ladder over this connection, so there is nothing to start from. You can still build one below and place it on the daemon host.</Notice> : null}
      {list || (ladder.data && !ladder.data.available) ? (
        <>
          <section class="sgroup">
            <div class="sgroup-head">
              <h2 class="sgroup-title">The ladder</h2>
              <div class="sgroup-actions">
                <button type="button" class="btn small" onClick={() => setAdding((v) => !v)} aria-expanded={adding}>
                  <Icon name="plus" /> Add provider
                </button>
              </div>
            </div>
            <div class="rows split-pane">
              <div class="pane-list" role="listbox" aria-label="Providers, in the order they are tried">
                {(list ?? []).map((p, i) => (
                  <div class={`pane-item ${i === sel ? 'on' : ''}`} role="option" aria-selected={i === sel} key={p.name + i} onClick={() => setSel(i)}>
                    <span class="ord">{i + 1}</span>
                    <span class="pi-text">
                      <span class="pi-name">{p.name || 'unnamed'}</span>
                      <span class="pi-meta mono">{p.kind}{p.model ? ` · ${p.model}` : ' · no model'}</span>
                    </span>
                    <Switch label={`${p.name} enabled`} checked={p.enabled} onChange={(on) => edit(i, { enabled: on })} />
                  </div>
                ))}
                {(list ?? []).length === 0 ? <div class="pane-empty">No provider. No model is ever called.</div> : null}
                <button type="button" class="pane-add" onClick={() => setAdding(true)}>
                  <Icon name="plus" /> Add provider
                </button>
              </div>
              <div class="pane-detail">
                {cur ? (
                  <>
                    <div class="pane-head">
                      <div>
                        <div class="title">{cur.name || 'unnamed'}</div>
                        <div class="desc">
                          {cur.enabled ? (cur.model ? 'Tried in position ' + (sel + 1) + ' for any operator that needs ' + cur.capabilities.join(', ') : 'Enabled, but no model is named yet') : 'Disabled: never called'}
                        </div>
                      </div>
                      <div class="pane-tools">
                        <button type="button" class="icon-btn" aria-label="Move up" title="Try earlier" disabled={sel === 0} onClick={() => move(sel, -1)}>
                          <Icon name="up" />
                        </button>
                        <button type="button" class="icon-btn" aria-label="Move down" title="Try later" disabled={sel === (list ?? []).length - 1} onClick={() => move(sel, 1)}>
                          <Icon name="down" />
                        </button>
                        <button type="button" class="icon-btn" aria-label="Remove provider" title="Remove" onClick={() => remove(sel)}>
                          <Icon name="trash" />
                        </button>
                      </div>
                    </div>
                    <div class="fields">
                      <label class="field">
                        Name
                        <input class="mono" value={cur.name} onInput={(e) => edit(sel, { name: (e.target as HTMLInputElement).value })} />
                      </label>
                      <label class="field">
                        Kind
                        <select
                          value={cur.kind}
                          onChange={(e) => {
                            const kind = (e.target as HTMLSelectElement).value as LadderProvider['kind']
                            edit(sel, { kind, capabilities: defaultCapabilities(kind), ...(DEFAULT_BASE[kind] && !cur.base_url ? { base_url: DEFAULT_BASE[kind] as string } : {}) })
                          }}
                        >
                          {KINDS.map((k) => (
                            <option value={k}>{k}</option>
                          ))}
                        </select>
                      </label>
                      <label class="field wide">
                        Base URL
                        <input class="mono" value={cur.base_url} onInput={(e) => edit(sel, { base_url: (e.target as HTMLInputElement).value })} />
                      </label>
                      <label class="field wide">
                        Model
                        <input class="mono" value={cur.model} placeholder="from the vendor's model list" onInput={(e) => edit(sel, { model: (e.target as HTMLInputElement).value })} />
                      </label>
                      <label class="field wide">
                        Key secret {NEEDS_KEY[cur.kind] ? <span class="req">required to enable</span> : <span class="muted">optional</span>}
                        <span class="inline">
                          <input class="mono" value={cur.api_key_secret ?? ''} placeholder={NEEDS_KEY[cur.kind] ? `${cur.kind}_api_key` : 'none'} onInput={(e) => edit(sel, { api_key_secret: (e.target as HTMLInputElement).value.trim() || undefined } as Partial<LadderProvider>)} />
                          {cur.api_key_secret ? have.has(cur.api_key_secret) ? <Chip tone="ok">stored</Chip> : <Chip tone="bad">not stored</Chip> : null}
                        </span>
                      </label>
                      <label class="field">
                        Input price, USD per million tokens
                        <input type="number" min={0} step="0.01" value={usd(cur.input_per_mtok_usd_micro)} onChange={(e) => edit(sel, { input_per_mtok_usd_micro: toMicro((e.target as HTMLInputElement).value) })} />
                      </label>
                      <label class="field">
                        Output price, USD per million tokens
                        <input type="number" min={0} step="0.01" value={usd(cur.output_per_mtok_usd_micro)} onChange={(e) => edit(sel, { output_per_mtok_usd_micro: toMicro((e.target as HTMLInputElement).value) })} />
                      </label>
                      <label class="field">
                        Max output tokens
                        <input type="number" min={1} max={1_000_000} value={cur.max_output_tokens} onChange={(e) => edit(sel, { max_output_tokens: Math.round(Number((e.target as HTMLInputElement).value)) })} />
                      </label>
                      <label class="field">
                        Timeout, seconds
                        <input type="number" min={1} max={600} value={Math.round(cur.timeout_ms / 1000)} onChange={(e) => edit(sel, { timeout_ms: Math.round(Number((e.target as HTMLInputElement).value)) * 1000 })} />
                      </label>
                      <div class="field wide">
                        Capabilities
                        <span class="inline caps">
                          {(['chat', 'json_schema', 'embeddings'] as const).map((c) => (
                            <label class="check">
                              <input type="checkbox" checked={cur.capabilities.includes(c)} onChange={(e) => edit(sel, { capabilities: (e.target as HTMLInputElement).checked ? [...cur.capabilities, c] : cur.capabilities.filter((x) => x !== c) })} />
                              {c}
                            </label>
                          ))}
                        </span>
                      </div>
                      {cur.base_url.startsWith('http://') && !isLoopbackUrl(cur.base_url) ? (
                        <label class="check wide">
                          <input type="checkbox" checked={cur.allow_insecure_http} onChange={(e) => edit(sel, { allow_insecure_http: (e.target as HTMLInputElement).checked })} />
                          Allow plain http to this host (no API key can be sent)
                        </label>
                      ) : null}
                    </div>
                    {own ? (
                      <KeyEntry required={NEEDS_KEY[cur.kind]} stored={!!cur.api_key_secret && have.has(cur.api_key_secret)} onSave={saveKey} onRemove={removeKey} />
                    ) : null}
                    {own && backend.agent && as ? (
                      <div class="stack-tight">
                        <label class="check wide">
                          <input type="checkbox" checked={consented.includes(cur.name)} onChange={(e) => toggleConsent(cur.name, (e.target as HTMLInputElement).checked)} />
                          Allow the agent to send prompts to <span class="mono">{providerHostOf(cur.base_url)}</span>
                        </label>
                        <div class="muted small-text">Nothing is sent to this provider until this is ticked. Workflows with llm operators are separate: they use this ladder only when run defaults allow model access, and need their own egress approval.</div>
                      </div>
                    ) : null}
                    {priceUnset(cur) ? <Notice kind="warn">The price is 0, so this provider's calls record no spend and no budget can stop them. Take the price from the vendor's list.</Notice> : null}
                    {!own && cur.api_key_secret && !have.has(cur.api_key_secret) ? (
                      <div class="stack-tight">
                        <div class="label">Store the key on a terminal</div>
                        <CodeCopy text={secretCommand(cur.api_key_secret)} label="Copy command" />
                        {backend.demo ? (
                          <button type="button" class="btn small" onClick={() => (backend.demo?.simulateSecret(cur.api_key_secret as string), secrets.reload())}>
                            Check again
                          </button>
                        ) : null}
                      </div>
                    ) : null}
                  </>
                ) : (
                  <div class="pane-empty">Choose a provider, or add one.</div>
                )}
              </div>
            </div>
            {showPresets ? (
              <div class="rows">
                <div class="row stack">
                  <div class="toolbar tight">
                    <Segmented
                      label="Region"
                      value={region}
                      onChange={setRegion}
                      options={[
                        ['all', 'All'],
                        ['global', 'Global'],
                        ['cn', 'China'],
                      ]}
                    />
                    <span class="muted">{presets.length} presets, from public documentation, not called from here</span>
                  </div>
                  <div class="preset-grid">
                    {presets.map((p) => (
                      <button type="button" class="preset" onClick={() => add(fromPreset(p, taken))}>
                        <span class="preset-name">{p.name}</span>
                        <span class="preset-meta">
                          <span class="mono">{providerHost(p)}</span>
                          {p.regions.includes('cn') ? <Chip>China</Chip> : null}
                          {p.secret === null ? <Chip tone="ok">local</Chip> : null}
                        </span>
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            ) : null}
          </section>
          {issues.length ? (
            <div class="notice warn" role="status">
              <strong>The daemon would refuse this ladder:</strong>
              <ul class="issues">
                {issues.slice(0, 8).map((i) => (
                  <li>
                    {i.index >= 0 ? <span class="mono">{list?.[i.index]?.name || `entry ${i.index + 1}`}.{i.field}</span> : null} {i.message}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          <SettingsGroup title="llm.json" note={own ? 'This is the ladder file (llm.json) in the state folder of your engine; Apply writes it through the engine. Keys are never in it, only the names of secrets.' : `Place this file in the daemon's state directory; ogund reads it at start. Studio cannot write it for you${backend.setLadder ? ', except here' : ''}. Keys are never in it, only the names of secrets.`}>
            <div class="row stack">
              <div class="toolbar tight">
                <CopyButton text={doc} label="Copy file" />
                <button type="button" class="btn small ghost" onClick={() => download('llm.json', doc + '\n', 'application/json')}>
                  <Icon name="download" /> Download
                </button>
                {backend.setLadder ? (
                  <button type="button" class="btn small primary" disabled={!dirty || issues.length > 0} onClick={() => void apply()}>
                    {own ? 'Apply to the engine' : 'Apply to this daemon'}
                  </button>
                ) : null}
                {applied ? <Chip tone="ok">applied</Chip> : null}
                {dirty ? <Chip tone="warn">unsaved changes</Chip> : null}
              </div>
              <pre class="code-block mono">{doc}</pre>
            </div>
          </SettingsGroup>
        </>
      ) : null}
      {own && backend.agent && as ? (
        <SettingsGroup title="Agent and spend limits" note="These apply to the agent screen. A cap of 0 means no model call at all; the daemon stops a thread at its cap and stops everything at the day's cap.">
          <SettingRow title="Offline" desc="No model call is made while this is on, whatever else is set up. Workflows without llm operators are not affected.">
            <Switch label="Offline" checked={as.offline} onChange={(on) => void setAgent({ offline: on })} />
          </SettingRow>
          <SettingRow title="Cap per thread" desc="Model spend in US dollars after which a thread stops calling the model.">
            <input type="number" min={0} max={1000} step={0.5} aria-label="Cap per thread in US dollars" value={as.spend_cap_usd} onChange={(e) => {
              const v = Number((e.target as HTMLInputElement).value)
              if (Number.isFinite(v) && v >= 0) void setAgent({ spend_cap_usd: v })
            }} />
          </SettingRow>
          <SettingRow title="Cap per day" desc="Model spend in US dollars across all threads in one UTC day. The ledger is the same one the llm operators write.">
            <input type="number" min={0} max={10000} step={1} aria-label="Cap per day in US dollars" value={as.day_cap_usd} onChange={(e) => {
              const v = Number((e.target as HTMLInputElement).value)
              if (Number.isFinite(v) && v >= 0) void setAgent({ day_cap_usd: v })
            }} />
          </SettingRow>
        </SettingsGroup>
      ) : null}
      <SettingsGroup title="Spend">
        {spend ? (
          <div class="row stack">
            <div class="text">
              <div class="title">This month</div>
              <div class="desc">
                ${(spend.month_usd_micro / 1_000_000).toFixed(2)} of a ${(spend.cap_usd_micro / 1_000_000).toFixed(2)} cap, from {fmtCount(spend.entries)} ledger entries. The ledger is a hash chain; a run over its budget stops for an approval.
              </div>
            </div>
            <div class="meter">
              <span style={{ width: `${Math.min(100, (spend.month_usd_micro / spend.cap_usd_micro) * 100).toFixed(1)}%` }} />
            </div>
          </div>
        ) : (
          <div class="row">
            <div class="text">
              <div class="desc">This daemon does not report spend over this connection. The ledger is <span class="mono">llm-ledger.jsonl</span> beside the ladder.</div>
            </div>
          </div>
        )}
      </SettingsGroup>
      <p class="muted">
        Documentation for the first preset in view: <ExternalLink href={(presets[0] ?? (PROVIDERS[0] as ProviderPreset)).docs_url} desktop={desktop} />
      </p>
    </>
  )
}
