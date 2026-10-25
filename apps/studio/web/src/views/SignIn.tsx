// Studio-web before sign-in: the server's sign-in providers, and, on the hosted pre-alpha, the shared sample
// account that anyone may use. Signing in happens on the identity provider's page (OIDC with PKCE); this page only
// links there.

import { useEffect, useState } from 'preact/hooks'

import { svcInfo, type SvcInfo } from '../api/svc'
import { Kv, PageHead } from '../ui/components'

interface Provider {
  id: string
  display_name: string
  login_url: string
}

export function SignIn() {
  const [providers, setProviders] = useState<Provider[] | null>(null)
  const [svc, setSvc] = useState<SvcInfo | null>(null)
  useEffect(() => {
    void fetch('/auth/providers', { credentials: 'same-origin', headers: { accept: 'application/json' } })
      .then((r) => (r.ok ? r.json() : null))
      .then((j: unknown) => {
        const list = Array.isArray(j) ? j : j && typeof j === 'object' && 'providers' in j ? (j as { providers: unknown }).providers : []
        setProviders(Array.isArray(list) ? (list as Provider[]).filter((p) => p && typeof p.login_url === 'string' && p.login_url.startsWith('/auth/login')) : [])
      })
      .catch(() => setProviders([]))
    void svcInfo().then(setSvc)
  }, [])
  const sample = svc?.visitors.sample_login && svc.visitors.sample_password ? svc.visitors : null
  return (
    <>
      <PageHead title="Sign in" sub="Studio-web shows this hub's workflows, runs and approvals to the people who sign in." />
      <div class="panel signin">
        {providers === null ? <p class="muted">Looking up the sign-in providers…</p> : null}
        {providers && providers.length === 0 ? <p>No sign-in provider is configured on this server.</p> : null}
        {providers?.map((p) => (
          <p>
            <a class="btn primary" href={p.login_url}>
              Sign in with {p.display_name}
            </a>
          </p>
        ))}
        {sample ? (
          <>
            <h3 style={{ marginTop: '20px' }}>Sample account</h3>
            <p>
              This hosted pre-alpha has one shared account on sample data. Anyone may use it, so do not enter anything
              private. What you store on the daemon is visible to other visitors and is reset {sample.reset}.
            </p>
            <Kv
              rows={[
                ['email', sample.sample_login ?? ''],
                ['password', sample.sample_password ?? ''],
              ]}
            />
            <p class="muted" style={{ fontSize: '13px' }}>
              Agent threads and the settings kept by the pre-alpha service are per browser and expire after{' '}
              {svc?.visitors.namespace_ttl_hours ?? 24} hours. Approvals on this hub are decided by a person at the host's
              terminal; no browser can decide one here.
            </p>
          </>
        ) : null}
      </div>
    </>
  )
}
