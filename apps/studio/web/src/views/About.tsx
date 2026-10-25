import { useEffect, useState } from 'preact/hooks'

import { svcInfo, type SvcInfo } from '../api/svc'
import { useBackend, useInfo } from '../lib/hooks'
import { Kv, PageHead } from '../ui/components'
import { Mark, VerneMark } from '../ui/icons'

export function About() {
  const { info } = useInfo()
  const web = info?.backend === 'http'
  const backend = useBackend()
  const [svc, setSvc] = useState<SvcInfo | null>(null)
  useEffect(() => void svcInfo().then(setSvc), [])
  const hosted = !!svc || (info?.link === 'hub' && !!backend.agent)
  return (
    <>
      <PageHead title="About Ogun Studio" />
      <div class="panel">
        <div class="brand" style={{ marginBottom: '16px', padding: 0 }}>
          <Mark />
          <div class="brand-name">
            OGUN STUDIO
            <small>version {info?.appVersion ?? __APP_VERSION__}</small>
          </div>
        </div>
        <p>
          Ogun is a local-first developer platform for data mining, data engineering, streams and agent benchmarks,
          built on the zeus-core engine library. Studio shows workflows, runs, partitions, approvals, artifacts,
          lineage and streams of an Ogun daemon or hub.
        </p>
        <Kv
          rows={[
            ['shell', info?.backend === 'demo' ? 'sample data, no daemon' : web ? 'Studio-web (ogun-server)' : `Tauri, ${info?.platform ?? ''}${info?.mobile ? ' (companion)' : ''}`],
            ...((info?.backend === 'demo' ? [['data', 'sample workflows and numbers; the agent answers from prepared sessions and no model is called']] : []) as [string, string][]),
            ['licence', 'Apache-2.0, open core'],
            ['typeface', 'the system interface font and the system monospace font; nothing is fetched'],
            ['platforms', 'as listed in O-12, Platform profiles'],
          ]}
        />
        {hosted ? (
          <>
            <h3 style={{ marginTop: '20px' }}>Pre-alpha release</h3>
            <p>
              This is the hosted pre-alpha hub. It runs on sample data in one shared workspace that is reset every night.
              Two programs answer Studio here, and every page says which one by what it can do:
            </p>
            <Kv
              rows={[
                ['ogund and ogun-server', 'workflows, runs, partitions, backfills, schedules, streams, edges, approvals, artifacts, lineage and the sign-in: the real daemon and server, version ' + (svc?.version ?? '0.0.1-alpha')],
                ['pre-alpha service', 'agent threads, connections, run defaults, the provider ladder, sources, evidence, storage rules and the archive: stored per browser for 24 hours; they do not change the daemon yet'],
                ['agent', 'its replies are prepared, no model is called; every tool it calls goes to the real daemon with an API token that acts as an agent, so it can never decide an approval'],
                ['code operators', 'not available: no operator sandbox is installed on this hub, so python, sql, dbt, shell and polars nodes cannot run, and their approvals stay pending'],
                ['approvals', 'decided only by a person at the host terminal; no browser or token can decide one here'],
                ['network', 'workflows reach only the sample file servers inside the hub (files.example.org, weather.example.org); nothing leaves the host'],
              ]}
            />
          </>
        ) : null}
        <div class="about-credit">
          <span>Designed and built by</span>
          <VerneMark />
          <span class="name">Verne Ltd.</span>
        </div>
        <p class="muted" style={{ marginTop: '8px', fontSize: '13px' }}>
          Copyright 2026 Verne Ltd. vernesystems.com
        </p>
      </div>
    </>
  )
}
