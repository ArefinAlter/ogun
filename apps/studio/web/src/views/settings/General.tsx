import { useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { OgunError, RuntimeSettings, RuntimeSettingsView } from '../../api/types'
import { idempotencyKey } from '../../lib/format'
import { useAsync, useBackend } from '../../lib/hooks'
import type { Notifications, TimeMode } from '../../lib/prefs'
import { resetPrefs, setPrefs, usePrefs } from '../../lib/prefs'
import { useScope } from '../../lib/scope'
import { fmtTime } from '../../lib/format'
import { Chip, ErrorBox, Notice, PageHead, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { ScopeBar } from '../../ui/ScopeBar'

export const START_PAGES: [string, string][] = [
  ['agent', 'Agent (Overview when the daemon has none)'],
  ['', 'Overview'],
  ['workflows', 'Workflows'],
  ['runs', 'Runs'],
  ['approvals', 'Approvals'],
  ['partitions', 'Partitions'],
  ['streams', 'Streams'],
]

type Key = keyof RuntimeSettings

/** A small marker that says a setting can be overridden per workflow, like T3 Code's layers icon beside a setting. */
function Layers({ on }: { on?: boolean }) {
  return (
    <span class={`layers ${on ? 'on' : ''}`} title={on ? 'This workflow overrides the default' : 'A workflow can override this'}>
      <Icon name="layers" />
    </span>
  )
}

function RuntimeGroups() {
  const backend = useBackend()
  const scope = useScope()
  const view = useAsync(() => (backend.runtime ? backend.runtime.get(scope.workflow || undefined) : Promise.resolve(null)), [backend, scope.workflow])
  const [error, setError] = useState<OgunError | undefined>()
  if (!backend.runtime)
    return (
      <Notice>
        This daemon does not expose run defaults over its API. They are set in its <span class="mono">config.json</span> and in each workflow's budgets and policy.
      </Notice>
    )
  const v = view.data as RuntimeSettingsView | null
  const apply = async (patch: Partial<RuntimeSettings>) => {
    setError(undefined)
    try {
      await backend.runtime?.set(scope.workflow || null, patch, idempotencyKey('rt'))
      view.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  const reset = async (field: Key) => {
    if (!scope.workflow) return
    try {
      await backend.runtime?.reset(scope.workflow, field, idempotencyKey('rt'))
      view.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  if (!v) return null
  const overridden = (k: Key) => v.overridden.includes(k)
  const title = (text: string, k: Key, overridable = true) => (
    <>
      {text} {overridable ? <Layers on={!!scope.workflow && overridden(k)} /> : null}
      {scope.workflow && overridden(k) ? (
        <>
          {' '}
          <button type="button" class="btn link" onClick={() => void reset(k)}>
            Reset to default
          </button>
        </>
      ) : null}
    </>
  )
  const num = (k: Key, min: number, max: number, unit?: string) => (
    <span class="withunit">
      <input type="number" aria-label={String(k)} min={min} max={max} value={Number(v.values[k])} onChange={(e) => void apply({ [k]: Math.min(max, Math.max(min, Math.round(Number((e.target as HTMLInputElement).value) || 0))) } as Partial<RuntimeSettings>)} />
      {unit ? <span class="muted">{unit}</span> : null}
    </span>
  )
  return (
    <>
      <ErrorBox error={error} title="Settings" />
      {scope.workflow ? (
        <Notice>
          You are editing <strong>{scope.workflow}</strong>. A setting shows the daemon-wide default until you change it here; <Chip tone="accent">overrides</Chip> marks the ones this workflow replaces.
        </Notice>
      ) : null}
      <SettingsGroup title="Run defaults" note="Applied to every run that does not say otherwise. A workflow's own budgets and policy win over these; changing a workflow's budgets in its document is a new version and may need an approval.">
        <SettingRow title={title('Concurrency', 'concurrency')} desc="Node instances that may run at once in one run.">
          {num('concurrency', 1, 64)}
        </SettingRow>
        <SettingRow title={title('Retries', 'retries')} desc="Attempts after a failure before a node instance fails. A node's own policy wins.">
          {num('retries', 0, 10)}
        </SettingRow>
        <SettingRow title={title('Wall-clock budget', 'wall_minutes')} desc="A run that exceeds it stops for a budget approval. 0 means no limit.">
          {num('wall_minutes', 0, 10_080, 'minutes')}
        </SettingRow>
        <SettingRow title={title('Bytes budget', 'bytes_gib')} desc="Total bytes a run may acquire. 0 means no limit.">
          {num('bytes_gib', 0, 4096, 'GiB')}
        </SettingRow>
        <SettingRow title={title('Approval expiry', 'approval_expiry_hours')} desc="A request nobody decides in this time expires; nothing proceeds on silence.">
          <select aria-label="Approval expiry" value={String(v.values.approval_expiry_hours)} onChange={(e) => void apply({ approval_expiry_hours: Number((e.target as HTMLSelectElement).value) })}>
            {[1, 4, 24, 72, 168, 720].map((h) => (
              <option value={String(h)}>{h < 24 ? `${h} hour${h > 1 ? 's' : ''}` : `${h / 24} day${h > 24 ? 's' : ''}`}</option>
            ))}
          </select>
        </SettingRow>
        <SettingRow title={title('Model access', 'model_access')} desc="Models are optional. With access off, a workflow that names an llm operator is refused before it starts.">
          <select aria-label="Model access" value={v.values.model_access} onChange={(e) => void apply({ model_access: (e.target as HTMLSelectElement).value as 'off' | 'ladder' })}>
            <option value="off">Off: no model is ever called</option>
            <option value="ladder">The provider ladder</option>
          </select>
        </SettingRow>
        {!scope.workflow ? (
          <SettingRow title="Resume on start" desc="After a restart or a SIGKILL, continue runs that were in flight. Finished partitions are never run again.">
            <Switch label="Resume on start" checked={v.values.resume_on_start} onChange={(on) => void apply({ resume_on_start: on })} />
          </SettingRow>
        ) : null}
      </SettingsGroup>
      <SettingsGroup title="Organization">
        <SettingRow title={title('Auto-pause failing schedules', 'auto_pause_after_failures')} desc="Pause a schedule after this many failed runs in a row. 0 means never; a person resumes it.">
          {num('auto_pause_after_failures', 0, 50, 'failures')}
        </SettingRow>
        <SettingRow title={title('Auto-archive finished runs', 'auto_archive_days')} desc="Set finished runs aside after this many days. Archived runs keep their hashes and lineage. 0 means never.">
          {num('auto_archive_days', 0, 3650, 'days')}
        </SettingRow>
        <SettingRow title={title('Keep runs that need attention', 'keep_attention_runs')} desc="Never auto-archive a run with a failed or waiting partition.">
          <Switch label="Keep runs that need attention" checked={v.values.keep_attention_runs} onChange={(on) => void apply({ keep_attention_runs: on })} />
        </SettingRow>
      </SettingsGroup>
    </>
  )
}

export function GeneralSettings() {
  const p = usePrefs()
  const sample = '2026-10-24T13:15:02.123Z'
  return (
    <>
      <PageHead title="General" sub="How runs behave on this daemon, and how Studio behaves on this device.">
        <button type="button" class="btn" onClick={() => resetPrefs()}>
          Restore device defaults
        </button>
      </PageHead>
      <ScopeBar />
      <RuntimeGroups />
      <SettingsGroup title="Sandbox, egress and secrets" note="These are not settings. They are how Ogun is built, shown here so nobody wonders.">
        <SettingRow title={<>Operators run sandboxed <Chip>fixed</Chip></>} desc="Every child operator runs out of process under bubblewrap on Linux. Windows has no operator sandbox yet: child operators are refused there unless allow_unsandboxed is set in config.json, which Studio cannot change." />
        <SettingRow title={<>Egress is denied until approved <Chip>fixed</Chip></>} desc="A workflow reaches a host only after a person approves the egress for that workflow version. Benchmark runs have no network at all by default." />
        <SettingRow title={<>Secrets stay out of everything <Chip>fixed</Chip></>} desc="Provider keys, tokens and URL query strings never reach logs, events, lineage, errors, fixtures or model prompts. Studio shows secret names, never values." />
        <SettingRow title={<>Approvals are decided by people <Chip>fixed</Chip></>} desc="Only on a person's own trust surface (the terminal or Studio), bound to the request hash. No path an agent can reach decides one." />
      </SettingsGroup>
      <SettingsGroup title="This device">
        <SettingRow title="Start page" desc="The page Studio opens on when the address has no page in it.">
          <select aria-label="Start page" value={p.startPage} onChange={(e) => setPrefs({ startPage: (e.target as HTMLSelectElement).value })}>
            {START_PAGES.map(([id, label]) => (
              <option value={id}>{label}</option>
            ))}
          </select>
        </SettingRow>
        <SettingRow title="Times" desc={`Timestamps in tables and details. Example: ${fmtTime(sample)}`}>
          <select aria-label="Times" value={p.timeMode} onChange={(e) => setPrefs({ timeMode: (e.target as HTMLSelectElement).value as TimeMode })}>
            <option value="utc">UTC, as the daemon records them</option>
            <option value="local">This computer's time zone</option>
          </select>
        </SettingRow>
        <SettingRow title="Confirm before cancelling a run" desc="Cancelling stops every node instance that has not finished. Finished partitions stay committed.">
          <Switch label="Confirm before cancelling a run" checked={p.confirmCancel} onChange={(v) => setPrefs({ confirmCancel: v })} />
        </SettingRow>
        <SettingRow title="Follow the event stream" desc="Pages refresh as the daemon commits events. Turn off to refresh by hand.">
          <Switch label="Follow the event stream" checked={p.liveUpdates} onChange={(v) => setPrefs({ liveUpdates: v })} />
        </SettingRow>
        <SettingRow title="Notifications" desc="Toasts for daemon restarts, failed runs, decisions and window revisions.">
          <select aria-label="Notifications" value={p.notifications} onChange={(e) => setPrefs({ notifications: (e.target as HTMLSelectElement).value as Notifications })}>
            <option value="all">Everything</option>
            <option value="important">Failures and decisions only</option>
            <option value="off">None</option>
          </select>
        </SettingRow>
      </SettingsGroup>
    </>
  )
}
