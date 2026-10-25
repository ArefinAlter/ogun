import { useEffect, useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { AgentAccess, AgentSettings, OgunError } from '../../api/types'
import { ACCESS_INFO, ACCESS_ORDER, DEFAULT_INSTRUCTIONS, NEVER, TOOLS } from '../../lib/agentTools'
import { useAgent, useAgentModels, useAgentSettings } from '../../lib/agentHooks'
import { idempotencyKey } from '../../lib/format'
import { href } from '../../lib/hooks'
import { Chip, ErrorBox, Loading, Notice, PageHead, SettingRow, SettingsGroup, Switch } from '../../ui/components'

const MAX_INSTRUCTIONS = 8000

/** Text that looks like a credential. Instructions go into the agent's context, and no secret may. */
const SECRETISH = [
  /sk-[A-Za-z0-9_-]{16,}/,
  /ogt1_[A-Za-z0-9_]{10,}/,
  /AKIA[0-9A-Z]{12,}/,
  /-----BEGIN [A-Z ]*PRIVATE KEY-----/,
  /\b(password|passwd|secret|token|api[_-]?key)\s*[:=]\s*\S{6,}/i,
  /[?&](token|key|signature|sig|x-amz-signature)=[^\s&]{8,}/i,
]

export function looksSecret(text: string): boolean {
  return SECRETISH.some((r) => r.test(text))
}

export function AgentSettingsPage() {
  const agent = useAgent()
  const settings = useAgentSettings()
  const models = useAgentModels()
  const [error, setError] = useState<OgunError | undefined>()
  const [text, setText] = useState('')
  const [saved, setSaved] = useState(false)
  const s = settings.data
  useEffect(() => {
    if (s) setText(s.instructions)
  }, [s?.instructions])

  const set = async (patch: Partial<AgentSettings>) => {
    setError(undefined)
    try {
      await agent?.setSettings(patch, idempotencyKey('agent-settings'))
      settings.reload()
      return true
    } catch (e) {
      setError(asOgunError(e))
      return false
    }
  }

  if (!agent)
    return (
      <>
        <PageHead title="Agent" sub="Instructions, access and limits for the conversation screen." />
        <Notice>
          This daemon does not run agent threads (O-17 is a proposal), so there is nothing to configure here. An MCP client such as Claude Code can use Ogun's tools today; <a href={href('agent')}>the Agent page</a> shows the two commands.
        </Notice>
      </>
    )

  const secret = looksSecret(text)
  const changed = s ? text !== s.instructions : false
  const toggleTool = (name: string, on: boolean) => {
    if (!s) return
    void set({ tools_enabled: on ? [...new Set([...s.tools_enabled, name])] : s.tools_enabled.filter((t) => t !== name) })
  }
  return (
    <>
      <PageHead title="Agent" sub="Instructions, access and limits for the conversation screen." />
      <ErrorBox error={settings.error} />
      <ErrorBox error={error} title="Agent" />
      {settings.loading && !s ? <Loading /> : null}
      {s ? (
        <>
          <SettingsGroup
            title="Instructions"
            note="Standing instructions for every thread, like an AGENTS.md. They shape what the agent says and proposes. They cannot give it a tool, an access level or a permission it does not have."
          >
            <SettingRow
              stack
              title={
                <>
                  Instructions <Chip>{text.length} of {MAX_INSTRUCTIONS}</Chip> {secret ? <Chip tone="bad">looks like a secret</Chip> : null}
                </>
              }
              desc="Instructions travel with every request the agent makes, so they must never hold a secret, a token or a URL with a query string. Name a secret instead: the agent can see names, never values."
            >
              <textarea
                class="instructions mono"
                aria-label="Instructions"
                rows={9}
                maxLength={MAX_INSTRUCTIONS}
                value={text}
                onInput={(e) => {
                  setSaved(false)
                  setText((e.target as HTMLTextAreaElement).value)
                }}
              />
              <div class="decide-row">
                <button type="button" class="btn" disabled={text === DEFAULT_INSTRUCTIONS} onClick={() => (setSaved(false), setText(DEFAULT_INSTRUCTIONS))}>
                  Restore the default
                </button>
                <button
                  type="button"
                  class="btn primary"
                  disabled={!changed || secret}
                  onClick={async () => {
                    if (await set({ instructions: text })) setSaved(true)
                  }}
                >
                  Save instructions
                </button>
                {saved ? <span class="muted">Saved. New messages use it.</span> : null}
              </div>
            </SettingRow>
          </SettingsGroup>

          <SettingsGroup title="New threads">
            <SettingRow title="Default access" desc="What the agent may do in a thread you start. You can change it per thread, in the composer or the side panel.">
              <select aria-label="Default access" value={s.default_access} onChange={(e) => void set({ default_access: (e.target as HTMLSelectElement).value as AgentAccess })}>
                {ACCESS_ORDER.map((a) => (
                  <option value={a}>{ACCESS_INFO[a].label}</option>
                ))}
              </select>
            </SettingRow>
            <SettingRow title="Default model" desc="The entry of the provider ladder a new thread starts with. Models are optional: without one the agent screen is closed, and every workflow still runs.">
              <select aria-label="Default model" value={s.default_model} onChange={(e) => void set({ default_model: (e.target as HTMLSelectElement).value })}>
                {(models ?? []).map((m) => (
                  <option value={m.id} disabled={!m.available}>
                    {m.label}
                    {m.available ? '' : ' (not available)'}
                  </option>
                ))}
              </select>
            </SettingRow>
            <SettingRow title="Spend cap per thread" desc="Model spend in US dollars after which a thread stops calling the model. 0 means no model call at all. It is counted in the same spend ledger as the llm operators.">
              <input
                type="number"
                min={0}
                max={1000}
                step={0.5}
                aria-label="Spend cap per thread in US dollars"
                value={s.spend_cap_usd}
                onChange={(e) => {
                  const v = Number((e.target as HTMLInputElement).value)
                  if (Number.isFinite(v) && v >= 0) void set({ spend_cap_usd: v })
                }}
              />
            </SettingRow>
            <SettingRow title="Keep threads" desc="How long finished threads stay before they are removed. A thread holds your questions and the daemon's answers, never a secret.">
              <select aria-label="Keep threads" value={s.retention_days} onChange={(e) => void set({ retention_days: Number((e.target as HTMLSelectElement).value) })}>
                {[7, 30, 90, 365].map((d) => (
                  <option value={d}>{d} days</option>
                ))}
              </select>
            </SettingRow>
          </SettingsGroup>

          <SettingsGroup title="Tools the agent may call" note="A tool switched off here is refused in every thread, whatever its access. The list is closed: a tool that is not in it does not exist.">
            {TOOLS.map((t) => (
              <SettingRow
                title={
                  <>
                    <span class="mono">{t.name}</span> {t.access !== 'read' ? <Chip tone="warn">{ACCESS_INFO[t.access].label}</Chip> : null}
                  </>
                }
                desc={t.summary}
              >
                <Switch label={`Allow ${t.name}`} checked={s.tools_enabled.includes(t.name)} onChange={(on) => toggleTool(t.name, on)} />
              </SettingRow>
            ))}
          </SettingsGroup>

          <SettingsGroup title="What no setting changes" note="These are not settings. They are how Ogun is built, shown here so nobody wonders.">
            {NEVER.map((n) => (
              <SettingRow
                title={
                  <>
                    {n.split('.')[0]} <Chip>fixed</Chip>
                  </>
                }
                desc={n.split('.').slice(1).join('.').trim() || undefined}
              />
            ))}
          </SettingsGroup>
        </>
      ) : null}
    </>
  )
}
