import { useState } from 'preact/hooks'

import { asOgunError } from '../../api'
import type { EvidenceSettings, OgunError } from '../../api/types'
import { idempotencyKey } from '../../lib/format'
import { href, useAsync, useBackend } from '../../lib/hooks'
import { ErrorBox, Loading, Notice, PageHead, SettingRow, SettingsGroup, Switch } from '../../ui/components'

export function EvidenceSettingsPage() {
  const backend = useBackend()
  const ev = useAsync(() => (backend.evidence ? backend.evidence.get() : Promise.resolve(null)), [backend])
  const [error, setError] = useState<OgunError | undefined>()
  const set = async (patch: Partial<EvidenceSettings>) => {
    setError(undefined)
    try {
      await backend.evidence?.set(patch, idempotencyKey('ev'))
      ev.reload()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  const v = ev.data
  return (
    <>
      <PageHead title="Evidence" sub="One file per run that says what ran, which exact workflow, and where every artifact came from." />
      <Notice>
        Every byte has a lineage, and a bundle is how you hand that to someone else. Open a run and choose <strong>Download evidence</strong>. The bundle is assembled in this window from what the daemon reports and is never uploaded. It holds the workflow hash, the artifact digests and the OpenLineage events, and no secret.
      </Notice>
      <ErrorBox error={ev.error} />
      <ErrorBox error={error} title="Evidence" />
      {ev.loading && !v && backend.evidence ? <Loading /> : null}
      {!backend.evidence ? (
        <>
          <Notice>This daemon does not store evidence settings; the defaults below apply. Bundles still download from a run's page.</Notice>
          <SettingsGroup title="Defaults">
            <SettingRow title="Format" desc="JSON for tools, HTML for a person to print or file." />
            <SettingRow title="Lineage" desc="Included." />
            <SettingRow title="Artifact previews" desc="Not included." />
          </SettingsGroup>
        </>
      ) : null}
      {v ? (
        <SettingsGroup title="Run evidence">
          <SettingRow title="Offer evidence on run pages" desc="Shows the Download evidence button on every run.">
            <Switch label="Offer evidence on run pages" checked={v.enabled} onChange={(on) => void set({ enabled: on })} />
          </SettingRow>
          <SettingRow title="Format" desc="JSON for tools, HTML for a person to print or file. The HTML has no script in it.">
            <select aria-label="Format" value={v.format} disabled={!v.enabled} onChange={(e) => void set({ format: (e.target as HTMLSelectElement).value as 'json' | 'html' })}>
              <option value="json">JSON</option>
              <option value="html">HTML (printable)</option>
            </select>
          </SettingRow>
          <SettingRow title="Include lineage" desc="The OpenLineage run events for the run: inputs and outputs of each step.">
            <Switch label="Include lineage" checked={v.include_lineage} disabled={!v.enabled} onChange={(on) => void set({ include_lineage: on })} />
          </SettingRow>
          <SettingRow title="Include artifact previews" desc="The first 4 KiB of small text artifacts. Off by default: a preview is data, and the digest already identifies the file.">
            <Switch label="Include artifact previews" checked={v.include_previews} disabled={!v.enabled} onChange={(on) => void set({ include_previews: on })} />
          </SettingRow>
        </SettingsGroup>
      ) : null}
      <p class="muted">
        Try it: <a href={href('runs')}>open a run</a> and look for the button beside its title.
      </p>
    </>
  )
}
