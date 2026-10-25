import { useInfo } from '../lib/hooks'
import { Kv, PageHead } from '../ui/components'
import { Mark, VerneMark } from '../ui/icons'

export function About() {
  const { info } = useInfo()
  const web = info?.backend === 'http'
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
            ['shell', web ? 'Studio-web (ogun-server)' : `Tauri, ${info?.platform ?? ''}${info?.mobile ? ' (companion)' : ''}`],
            ['licence', 'Apache-2.0, open core'],
            ['fonts', 'IBM Plex Sans, IBM Plex Mono, EB Garamond (SIL Open Font License 1.1)'],
            ['platforms', 'as listed in O-12, Platform profiles'],
          ]}
        />
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
