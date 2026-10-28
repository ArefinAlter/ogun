import { useEffect, useState } from 'preact/hooks'

import { svcInfo, type SvcInfo } from '../api/svc'
import { useAsync, useBackend, useInfo } from '../lib/hooks'
import { Kv, PageHead } from '../ui/components'
import { Mark, VerneMark } from '../ui/icons'

export function About() {
  const { info } = useInfo()
  const web = info?.backend === 'http'
  const backend = useBackend()
  const [svc, setSvc] = useState<SvcInfo | null>(null)
  useEffect(() => void svcInfo().then(setSvc), [])
  const hosted = !!svc || (info?.link === 'hub' && !!backend.agent)
  // The desktop app's own engine: real data and a real agent with the person's selected provider.
  const own = !!backend.engine && !!info?.engine
  const sandbox = useAsync(() => (backend.engine ? backend.engine.sandboxStatus().catch(() => null) : Promise.resolve(null)), [backend])
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
            ...((own && info?.engine ? [['engine', `ogund ${info.engine.version ?? ''}, started and stopped by Studio; its data is in ${info.engine.state_dir}`]] : []) as [string, string][]),
            ...((info?.backend === 'demo' ? [['data', 'sample workflows and numbers; the agent answers from prepared sessions and no model is called']] : []) as [string, string][]),
            ['licence', 'Apache-2.0, open core'],
            ['typeface', 'the system interface font and the system monospace font; nothing is fetched'],
            ['platforms', 'as listed in O-12, Platform profiles'],
          ]}
        />
        {own ? (
          <>
            <h3 style={{ marginTop: '20px' }}>Alpha release</h3>
            <p>
              This is an alpha release. Studio is one installer with its own engine: nothing else to install or start, no account, and no network unless something you run uses it. What is real, and what is not:
            </p>
            <Kv
              rows={[
                ['data', 'yours, on this computer, in the state folder above. Workflows, runs, partitions, artifacts, lineage and schedules come from the real engine: the sample projects are workflows you can read and change, nothing is prepared in advance. Uninstalling Studio keeps the folder'],
                ['workflows', 'core operators (fetch, parse, filter, partition, hash, manifest and the rest) run inside the engine on real inputs. A workflow fetches only from the hosts it names'],
                ['schedules', 'fire while Studio is open, or while it keeps running in the tray if you turn that on. They do not fire when the engine is stopped'],
                ['code operators', sandbox.data ? sandbox.data.summary : 'checked when a run needs one: the engine runs python, sql, dbt, shell and polars nodes only where this computer offers a sandbox, and Settings, General says exactly what it isolates'],
                ['approvals', 'decided only by you, in Studio or on a terminal, with the facts the engine computed and the request hash they are bound to. No agent, token or schedule can decide one'],
                ['agent', 'answers with a model you choose, using your provider key or an officially signed-in local agent CLI. Nothing is sent to a provider until you allow it in Settings, Providers, and "What will be sent" shows each request first. Its words are not verified by Ogun, it has no tool that decides an approval, reads a secret or runs a shell, and every change it proposes is a button you press'],
                ['keys', 'typed once, kept by the engine in the credential store of this computer where it has one; never shown again, logged or put in a prompt'],
                ['hubs', 'optional. A hub is an Ogun server your team runs; on it you act with its own roles'],
                ['not in this release', 'macOS, Android and iOS builds; signed installers (Windows may warn that the app is unsigned); connectors marked manifest only in the catalogue'],
              ]}
            />
          </>
        ) : null}
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
