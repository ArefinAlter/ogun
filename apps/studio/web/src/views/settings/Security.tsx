import { fmtTime } from '../../lib/format'
import { useAsync, useBackend, useInfo } from '../../lib/hooks'
import { CodeCopy, Empty, ErrorBox, Loading, Notice, PageHead, SettingsGroup } from '../../ui/components'

export function SecuritySettings() {
  const backend = useBackend()
  const { info } = useInfo()
  const can = !!info?.canReadSettings
  const creds = useAsync(() => (can ? backend.credentialsList() : Promise.resolve(null)), [backend, can])
  const secrets = useAsync(() => (can ? backend.secretsList() : Promise.resolve(null)), [backend, can])
  return (
    <>
      <PageHead title="Security" sub="Credentials and secrets are managed on a terminal, never in this window." />
      <Notice>
        Studio lists credentials and the <em>names</em> of secrets. It never shows, accepts or stores a secret value, and nothing an agent can reach decides an approval.
      </Notice>
      {!can ? (
        <Notice>Local credentials and secret names are listed only on a local desktop link with a human credential.</Notice>
      ) : (
        <>
          <SettingsGroup title="Credentials">
            <ErrorBox error={creds.error} />
            {creds.loading && !creds.data ? <Loading /> : null}
            {creds.data && creds.data.credentials.length === 0 ? <Empty>No credentials.</Empty> : null}
            {creds.data && creds.data.credentials.length > 0 ? (
              <div class="table-wrap flush">
                <table>
                  <thead>
                    <tr>
                      <th>Name</th>
                      <th>Kind</th>
                      <th>Id</th>
                      <th>Created</th>
                      <th>State</th>
                    </tr>
                  </thead>
                  <tbody>
                    {creds.data.credentials.map((c) => (
                      <tr>
                        <td class="mono">{c.name}</td>
                        <td class="mono">{c.kind}</td>
                        <td class="mono">{c.credential_id}</td>
                        <td>{fmtTime(c.created_at)}</td>
                        <td>{c.revoked ? <span class="state s-cancelled">revoked</span> : 'live'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : null}
          </SettingsGroup>
          <SettingsGroup title="Secret names" note={<>Names a workflow can use in <code>{'{"secret_ref": "<name>"}'}</code>. Values are never shown and never leave the daemon.</>}>
            <ErrorBox error={secrets.error} />
            {secrets.data && secrets.data.names.length === 0 ? <Empty>No secrets.</Empty> : null}
            {secrets.data && secrets.data.names.length > 0 ? (
              <div class="row stack">
                <ul class="mono namelist">
                  {secrets.data.names.map((n) => (
                    <li>{n}</li>
                  ))}
                </ul>
              </div>
            ) : null}
          </SettingsGroup>
        </>
      )}
      <SettingsGroup title="On a terminal">
        <div class="row stack">
          <div class="text">
            <div class="title">Create an agent credential for an integration</div>
            <div class="desc">Airflow, the exporters, MCP and scripts use the agent kind. It reads, authors workflows and starts runs; it can never approve.</div>
          </div>
          <CodeCopy text="ogun credentials add airflow --kind agent" />
        </div>
        <div class="row stack">
          <div class="text">
            <div class="title">Store a secret</div>
            <div class="desc">The value is read from standard input, or typed without echo.</div>
          </div>
          <CodeCopy text="ogun secrets set <name>" />
        </div>
        <div class="row stack">
          <div class="text">
            <div class="title">Decide an approval</div>
            <div class="desc">On the terminal of the machine that runs the daemon, or in Studio on the desktop with the pinned binary.</div>
          </div>
          <CodeCopy text="ogun approvals" />
        </div>
      </SettingsGroup>
    </>
  )
}
