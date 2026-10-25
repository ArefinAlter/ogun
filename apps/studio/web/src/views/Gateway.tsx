// The Android field gateway (O-12 section 3): collect from LAN sensors in a foreground service, keep a durable
// ingest log, sync sealed segments to a hub on an unmetered network (and while charging, by default).

import { useEffect, useState } from 'preact/hooks'

import { asOgunError } from '../api'
import type { GatewaySettings, GatewayStatus } from '../api/tauri'
import { gateway } from '../api/tauri'
import type { OgunError } from '../api/types'
import { fmtBytes, fmtCount, fmtMs } from '../lib/format'
import { useAsync } from '../lib/hooks'
import { ErrorBox, Hash, Kv, Loading, Notice, PageHead } from '../ui/components'

const DEFAULTS: GatewaySettings = {
  name: 'phone',
  mqtt_topics: [],
  http_push_enabled: false,
  http_push_port: 8787,
  sync_requires_charging: true,
  segment_ms: 300_000,
  max_log_bytes: 512 * 1024 * 1024,
  when_full: 'block',
}

export function Gateway() {
  const status = useAsync(() => gateway.status(), [])
  const [error, setError] = useState<OgunError | undefined>()
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    const t = setInterval(() => status.reload(), 5000)
    return () => clearInterval(t)
  }, [status.reload])
  const act = async (f: () => Promise<GatewayStatus>) => {
    setBusy(true)
    setError(undefined)
    try {
      await f()
      status.reload()
    } catch (e) {
      setError(asOgunError(e))
    } finally {
      setBusy(false)
    }
  }
  const s = status.data
  const c = s?.core
  return (
    <>
      <PageHead title="Field gateway">
        {s ? (
          s.running ? (
            <button type="button" class="btn" disabled={busy} onClick={() => act(gateway.stop)}>
              Stop collecting
            </button>
          ) : (
            <button type="button" class="btn primary" disabled={busy} onClick={() => act(gateway.start)}>
              Start collecting
            </button>
          )
        ) : null}
        <button type="button" class="btn" disabled={busy} onClick={() => act(gateway.syncNow)}>
          Sync now
        </button>
      </PageHead>
      <Notice>
        The gateway runs as a visible foreground service. Android may stop it, Doze defers network while the phone is
        idle, and Android 15 limits data-sync services to 6 hours a day; the service stops itself at that limit and
        the MQTT broker keeps queued messages until it reconnects. Sync uses an unmetered network only.
      </Notice>
      <ErrorBox error={status.error} />
      <ErrorBox error={error} />
      {status.loading && !s ? <Loading /> : null}
      {s ? (
        <div class="panel">
          <h2>Status</h2>
          <Kv
            rows={[
              ['service', s.running ? 'collecting' : s.stopped_reason ? `stopped: ${s.stopped_reason}` : 'stopped'],
              ['MQTT', s.mqtt ? `${s.mqtt.connected ? 'connected' : 'disconnected'} · ${fmtCount(s.mqtt.received)} received${s.mqtt.last_error ? ` · ${s.mqtt.last_error}` : ''}` : 'not configured'],
              ['HTTP push', s.http_push ? `${s.http_push.listening ? `listening on ${s.http_push.address ?? ''}` : 'off'} · ${fmtCount(s.http_push.received)} received` : 'off'],
              ...(c
                ? ([
                    ['records stored', fmtCount(c.stored_records)],
                    ['log', `${fmtBytes(c.log.bytes_used)} · next seq ${fmtCount(c.log.next_seq)}`],
                    ['losses', `dropped ${fmtCount(c.log.dropped_newest)} · lost ${fmtCount(c.log.lost_oldest)} · invalid ${fmtCount(c.invalid)}`],
                    ['segments waiting', `${fmtCount(c.pending_segments)} (${fmtBytes(c.pending_bytes)})`],
                    ['segments synced', `${fmtCount(c.acked_segments)} (${fmtBytes(c.acked_bytes)})`],
                    ['last seal', c.last_seal_ms ? fmtMs(c.last_seal_ms) : 'never'],
                    ['last sync', c.last_sync ? `${fmtMs(c.last_sync.at_ms)} · ${c.last_sync.ok ? 'ok' : 'failed'}${c.last_sync.reason ? ` · ${c.last_sync.reason}` : ''}` : 'never'],
                  ] as [string, string][])
                : []),
            ]}
          />
          <ErrorBox error={s.core_error} title="Gateway core" />
        </div>
      ) : null}
      {c ? (
        <div class="panel">
          <h2>Enrolment</h2>
          <p class="muted">
            A person enrols this gateway's public key on the hub host (the hub's <code>edges.enroll</code>, on its
            terminal), compares the fingerprint on both screens, and enters the <code>edg_</code> id the hub returns
            below. Until then nothing is synced.
          </p>
          <Kv
            rows={[
              ['edge id', c.edge_id ? <span class="mono">{c.edge_id}</span> : 'not enrolled'],
              ['public key', <Hash value={c.public_key} />],
              ['fingerprint', <span class="mono">{c.fingerprint}</span>],
              ['asset', <span class="mono">{c.asset}</span>],
            ]}
          />
        </div>
      ) : null}
      {s ? <GatewayForm current={s} onSaved={() => status.reload()} /> : null}
    </>
  )
}

function GatewayForm({ current, onSaved }: { current: GatewayStatus; onSaved: () => void }) {
  const init: GatewaySettings = { ...DEFAULTS, ...current.settings }
  const [f, setF] = useState<GatewaySettings>(init)
  const [topics, setTopics] = useState(init.mqtt_topics.join('\n'))
  const [password, setPassword] = useState('')
  const [pushToken, setPushToken] = useState('')
  const [error, setError] = useState<OgunError | undefined>()
  const [saved, setSaved] = useState(false)
  const set = <K extends keyof GatewaySettings>(k: K, v: GatewaySettings[K]) => setF((o) => ({ ...o, [k]: v }))
  const save = async () => {
    setError(undefined)
    setSaved(false)
    try {
      await gateway.configure({
        ...f,
        mqtt_topics: topics.split(/\s+/).filter(Boolean),
        ...(password ? { mqtt_password: password } : {}),
        ...(pushToken ? { http_push_token: pushToken } : {}),
      })
      setPassword('')
      setPushToken('')
      setSaved(true)
      onSaved()
    } catch (e) {
      setError(asOgunError(e))
    }
  }
  const input = (k: keyof GatewaySettings, label: string, placeholder = '') => (
    <label class="field">
      {label}
      <input
        value={(f[k] as string | undefined) ?? ''}
        placeholder={placeholder}
        onInput={(e) => set(k, (e.target as HTMLInputElement).value as never)}
      />
    </label>
  )
  return (
    <form
      class="panel"
      onSubmit={(e) => {
        e.preventDefault()
        void save()
      }}
    >
      <h2>Configuration</h2>
      <h3>Identity and hub</h3>
      <div class="form-row">
        {input('name', 'Gateway name', 'phone-1')}
        {input('edge_id', 'Edge id from the hub', 'edg_…')}
        {input('hub_url', 'Hub URL', 'https://hub.example.org')}
      </div>
      <label class="field" style={{ marginBottom: '12px' }}>
        Private CA (PEM), for the hub and the broker; leave empty for public certificates
        <textarea value={f.hub_ca_pem ?? ''} style={{ minHeight: '80px' }} onInput={(e) => set('hub_ca_pem', (e.target as HTMLTextAreaElement).value)} />
      </label>
      <h3>MQTT subscription</h3>
      <div class="form-row">
        {input('mqtt_url', 'Broker', 'mqtts://broker.local:8883')}
        {input('mqtt_username', 'Username')}
        <label class="field">
          Password {current.settings.mqtt_password_set ? '(stored; blank keeps it)' : ''}
          <input type="password" autocomplete="off" value={password} onInput={(e) => setPassword((e.target as HTMLInputElement).value)} />
        </label>
      </div>
      <label class="field" style={{ marginBottom: '12px' }}>
        Topics, one per line (QoS 1)
        <textarea value={topics} style={{ minHeight: '70px' }} onInput={(e) => setTopics((e.target as HTMLTextAreaElement).value)} />
      </label>
      <h3>HTTP push on the local network</h3>
      <div class="form-row">
        <label class="check">
          <input type="checkbox" checked={f.http_push_enabled} onChange={(e) => set('http_push_enabled', (e.target as HTMLInputElement).checked)} />
          accept POST /ingest from devices on this network
        </label>
        <label class="field">
          Port
          <input type="number" min={1024} max={65535} value={f.http_push_port} onInput={(e) => set('http_push_port', Number((e.target as HTMLInputElement).value))} />
        </label>
        <label class="field">
          Device token {current.settings.http_push_token_set ? '(stored; blank keeps it)' : ''}
          <input type="password" autocomplete="off" value={pushToken} onInput={(e) => setPushToken((e.target as HTMLInputElement).value)} />
        </label>
      </div>
      <h3>Log and sync</h3>
      <div class="form-row">
        <label class="field">
          Segment length (s)
          <input type="number" min={10} max={3600} value={Math.round(f.segment_ms / 1000)} onInput={(e) => set('segment_ms', Number((e.target as HTMLInputElement).value) * 1000)} />
        </label>
        <label class="field">
          Log limit (MiB)
          <input type="number" min={1} value={Math.round(f.max_log_bytes / 1048576)} onInput={(e) => set('max_log_bytes', Number((e.target as HTMLInputElement).value) * 1048576)} />
        </label>
        <label class="field">
          When the log is full
          <select value={f.when_full} onChange={(e) => set('when_full', (e.target as HTMLSelectElement).value as GatewaySettings['when_full'])}>
            <option value="block">block (stop acknowledging)</option>
            <option value="drop_oldest">drop oldest</option>
            <option value="drop_newest">drop newest</option>
          </select>
        </label>
        <label class="check">
          <input type="checkbox" checked={f.sync_requires_charging} onChange={(e) => set('sync_requires_charging', (e.target as HTMLInputElement).checked)} />
          sync only while charging
        </label>
      </div>
      <div class="toolbar">
        <button type="submit" class="btn primary">
          Save
        </button>
        {saved ? <span class="muted">Saved. Restart collecting to apply broker changes.</span> : null}
      </div>
      <ErrorBox error={error} />
    </form>
  )
}
