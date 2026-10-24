# Self-hosting Ogun

This guide sets up the Ogun server profile: one `ogund` daemon and the `ogun-server` bridge, so that several people use Ogun from a browser, agents reach it over HTTP, and edges sync to it. The contract behind it is [O-13](spec/O-13-server-mode.md); the decision is [ADR 0010](adr/0010-self-hosted-server-mode.md).

Ogun is pre-alpha. Expect changes between versions.

## What you get, and what you do not

- **One installation, one replica.** The daemon's run store is a single-writer database, so there is no high availability and no horizontal scaling. Plan for backups, not failover.
- **Linux only** for the server profile (the O-12 platform matrix): a container, a Kubernetes pod or a Linux host.
- **Sign-in through your identity provider** (OpenID Connect). There are no passwords. Nobody gets an account without an invitation link.
- **Agents use API tokens.** A token always acts as an agent: it can read, author workflows and start runs, and it can never decide an approval.
- **Approvals are decided on the host by default**, with `ogun approvals decide` on a terminal. Deciding from the browser with a passkey ("remote approvals") is built but switched off: ADR 0010 requires an external security review before it is released. Leave `remote_approvals` off.

## How it fits together

```text
browser, agents, edges
        |  HTTPS
   [reverse proxy, optional]
        |
   ogun-server  ---- local socket ---->  ogund --profile server
   (OIDC, roles, Studio-web, REST,       (runs, schedules, approvals,
    MCP, edge sync relay)                 artifacts; no network listener)
```

`ogund` never listens on the network. `ogun-server` connects to its socket with a credential of kind `bridge`, which a person creates on the host. Both run as the same user and share the state directory: the daemon's state is in `profiles/server`, the bridge's own store in `ogun-server`, both under `/var/lib/ogun` in the container image.

## Before you start

1. **A host name**, for example `ogun.example.com`, pointing at the machine or the cluster's ingress.
2. **TLS.** Either let Caddy or your ingress controller obtain a certificate, or give `ogun-server` a certificate and key in PEM files.
3. **An OIDC client** at your identity provider (Keycloak, Authelia, Microsoft Entra ID, Google, and others):
   - redirect URI: `https://ogun.example.com/auth/callback`;
   - scopes: `openid email profile`;
   - a confidential client with a client secret, or a public client (PKCE is always used);
   - note the issuer URL exactly as the provider's discovery document states it (`.../.well-known/openid-configuration`, member `issuer`).

## Option A: Docker Compose

The image is built from source, so check out Ogun and `zeus-core` side by side:

```sh
mkdir ogun-src && cd ogun-src
git clone https://github.com/ArefinAlter/ogun
git clone https://github.com/ArefinAlter/zeus-core
cd ogun/deploy
```

Or set `OGUN_IMAGE` to a released image (`ghcr.io/arefinalter/ogun:<version>`) and skip the build.

### With Caddy in front (recommended)

1. Edit `compose/server.proxy.json`: set `public_url` to your origin, and the OIDC provider's `issuer`, `client_id` and `display_name`.
2. Put the client secret in `compose/secrets/oidc-client-secret`, readable by uid 10001:

   ```sh
   printf '%s\n' 'the-secret' > compose/secrets/oidc-client-secret
   sudo chown 10001:10001 compose/secrets/oidc-client-secret && chmod 0400 compose/secrets/oidc-client-secret
   ```

3. Start the daemon and create the bridge credential. This needs a terminal (`-it`): the daemon lets only a person at a terminal create credentials.

   ```sh
   export OGUN_SERVER_JSON=./compose/server.proxy.json OGUN_DOMAIN=ogun.example.com
   docker compose --profile caddy up -d ogund
   docker compose exec -it ogund ogun --profile server credentials add server --kind bridge
   ```

4. Start the rest:

   ```sh
   docker compose --profile caddy up -d
   ```

5. On its first start `ogun-server` creates a workspace named "Main" and writes a one-time owner enrolment link to a file. Read it, open it in your browser, sign in, then delete the file:

   ```sh
   docker compose exec ogun-server cat /var/lib/ogun/ogun-server/owner-enrolment.txt
   docker compose exec ogun-server rm /var/lib/ogun/ogun-server/owner-enrolment.txt
   ```

### Without a proxy

`ogun-server` can terminate TLS itself: put `fullchain.pem` and `privkey.pem` in `compose/tls/`, edit `compose/server.tls.json` (its `public_url` includes the port, `:8443` by default), leave `OGUN_SERVER_JSON` unset and run the same steps without `--profile caddy`.

### Stream ingest

`docker compose --profile ingest up -d` adds the ingest bridge. It needs its own credential of kind `service` (`ogun --profile server credentials add ingest --kind service`) and its configuration in `profiles/server/ingest.json` on the volume.

## Option B: Kubernetes with Helm

The chart in `deploy/helm/ogun` runs one pod with the daemon and the bridge as two containers. They share a persistent volume for state and an in-memory volume for the socket. TLS ends at your ingress controller.

1. Create a Secret with the OIDC client secret:

   ```sh
   kubectl create secret generic ogun-oidc --from-literal=sso-client-secret='the-secret'
   ```

2. Write a values file:

   ```yaml
   publicUrl: https://ogun.example.com
   oidc:
     existingSecret: ogun-oidc
     providers:
       - id: sso
         displayName: Company sign-in
         issuer: https://login.example.com/realms/main
         clientId: ogun
         clientSecretKey: sso-client-secret
   server:
     # The addresses your ingress controller's pods connect from.
     trustedProxies: ["10.0.0.0/8"]
   ingress:
     className: nginx
     annotations:
       nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"
       nginx.ingress.kubernetes.io/proxy-buffering: "off"
     tls:
       - secretName: ogun-tls
   ```

3. Install, then follow the printed notes (bridge credential, owner link):

   ```sh
   helm install ogun ./deploy/helm/ogun -f my-values.yaml
   kubectl exec -it ogun-0 -c ogund -- ogun --profile server credentials add server --kind bridge
   kubectl exec ogun-0 -c ogun-server -- cat /var/lib/ogun/ogun-server/owner-enrolment.txt
   ```

`trustedProxies` matters: `ogun-server` closes every connection that does not come from those addresses, and takes the client address for rate limiting from `X-Forwarded-For` only behind them.

## Option C: a Linux host with systemd

1. Install the binaries (they are in the Linux release archives; `deploy/install.sh` verifies the checksums):

   ```sh
   sh deploy/install.sh --prefix /usr/local     # as root; installs into /usr/local/bin
   ```

2. Create the `ogun` user and install the units:

   ```sh
   install -Dm0644 deploy/systemd/system/ogun.sysusers.conf /usr/lib/sysusers.d/ogun.conf && systemd-sysusers
   install -Dm0644 deploy/systemd/system/ogund-server.service /etc/systemd/system/
   install -Dm0644 deploy/systemd/system/ogun-server.service /etc/systemd/system/
   systemctl daemon-reload && systemctl enable --now ogund-server
   ```

3. Create the bridge credential on a terminal, as the `ogun` user:

   ```sh
   sudo -u ogun env OGUN_HOME=/var/lib/ogun ogun --profile server credentials add server --kind bridge
   ```

4. Write `/etc/ogun/server.json` (see the members below; `client_secret_file` is `/run/credentials/ogun-server.service/oidc-client-secret`), put the client secret in `/etc/ogun/oidc-client-secret` (mode 0600, root), check it and create the first workspace while the server is stopped:

   ```sh
   ogun-server --config /etc/ogun/server.json check-config
   sudo -u ogun env OGUN_SERVER_HOME=/var/lib/ogun/ogun-server ogun-server --config /etc/ogun/server.json init
   systemctl enable --now ogun-server
   ```

   `init` prints the owner enrolment link once.

For a personal daemon on a workstation, `deploy/systemd/user/ogund.service` is a user unit (`systemctl --user enable --now ogund`).

## The configuration file

`server.json` is plain JSON; unknown members are errors, so typos do not pass silently. It never holds a secret. The members are listed in O-13 section 10. The ones you will set:

| Member | Example | Notes |
|---|---|---|
| `public_url` | `https://ogun.example.com` | The origin people type. No path. |
| `listen` | `0.0.0.0:8443` | `0.0.0.0:8080` behind a proxy. |
| `tls` | `{"cert_file": "...", "key_file": "..."}` | Omit behind a proxy. |
| `behind_proxy`, `trusted_proxies` | `true`, `["172.30.0.10/32"]` | Behind a proxy only. |
| `oidc.providers` | see the compose files | One entry per identity provider. |
| `sync.enabled` | `true` | Only when edges sync to this server. |

Run `ogun-server check-config` after every change. `serve` refuses to start on the same errors.

## Using it

- **People.** Owners and admins invite people from Studio-web; each invitation link works once, for 72 hours by default. Roles: viewer (read), editor (author, run, backfill), admin (pause workflows, manage viewers and editors), owner (manage admins, owners and the approver flag).
- **API tokens** are created in Studio-web by a signed-in person, shown once, and act as agents. Use them for scripts and integrations: `curl -H "Authorization: Bearer ogt1_..." https://ogun.example.com/api/v1/workflows`.
- **MCP.** Agents connect to `https://ogun.example.com/mcp` with a token. For Claude Code: `claude mcp add --transport http ogun https://ogun.example.com/mcp --header "Authorization: Bearer ogt1_..."`. The tools list and read workflows, validate, start runs with an idempotency key, and read run, partition and approval status. They never decide approvals.
- **Approvals.** A person decides on the host: `ogun --profile server approvals list` and `ogun --profile server approvals decide ...` on a terminal (in a container, through `exec -it`).
- **Edges.** Enrol each edge on the host terminal, set `sync.enabled` to `true`, and point the edge at `https://ogun.example.com/api/v1/sync/`.

## Backups and upgrades

- Back up the whole state volume (`/var/lib/ogun`): the daemon's redb files, its artifact store, its credentials and the bridge's store. Take the copy while the daemon is stopped or quiesced (ADR 0010).
- Upgrade by replacing the image or the binaries and restarting both processes. Read the release notes first while Ogun is pre-alpha.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| Every API call answers 503 "cannot reach ogund" | The daemon is not running, or the `server` bridge credential is missing (`check-config` warns about it). |
| 403 "unexpected Host header" | `public_url` does not match the name in the browser, or the proxy rewrites `Host`; add the name to `allowed_hosts` if it is legitimate. |
| Connections close at once behind a proxy | The proxy's address is not in `trusted_proxies`. |
| Sign-in fails with "names another issuer" | `issuer` must match the provider's discovery document exactly, including any trailing slash. |
| "there is no account for this identity" | The person needs an invitation link first. |
| The event stream stops after a while | Raise the proxy's read timeout and turn off response buffering for `/api/v1/events`. |

Logs go to standard error; set `OGUN_LOG=debug` for more. They never contain tokens, cookies, client secrets or enrolment links.
