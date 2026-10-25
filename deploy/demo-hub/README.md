# Demo hub: a real Ogun hub that listens on loopback only

This is the server profile of [self-hosting](../../docs/self-hosting.md) cut down to run next to other services on
one machine, for demonstrations and for trying Studio against the real daemon. It is not a production setup.

| Container | What |
|---|---|
| `ogund` | The real daemon, `ogund --profile server`. Its one listener is a Unix socket in a volume. |
| `ogun-server` | The real HTTP bridge: OIDC sign-in, sessions, API tokens, Studio-web, REST, MCP. |
| `dex` | An OpenID Connect provider with one static user, so sign-in works without an external identity provider. |

What keeps it contained:

- Both published ports are bound to `127.0.0.1` on the host (`18443` Studio-web and the API, `18444` the sign-in
  page). Nothing listens on a public interface and nothing touches the host's reverse proxy or firewall.
- You reach it through an SSH tunnel. The browser and the server agree on `http://localhost:...` because HTTP is
  allowed on a loopback host (O-13 section 10) and Dex shares `ogun-server`'s network namespace.
- Every container is read-only, drops all capabilities, cannot gain privileges, and has memory, CPU and process
  limits. They run on a private network, `172.30.0.0/24`.
- Secrets are files on the host (mode 0400 or 0600), never in this directory's tracked files or in `server.json`.

## What it cannot do

- **No operator sandbox.** The image has no bubblewrap and the container has no capabilities, so a workflow with a
  code operator (`python`, `sql`, `dbt`, `shell`, `polars`) cannot run here. Do not approve a code change on this
  hub: the run would fail with `op.sandbox_unavailable`. Core operators run for real; `workflows/` holds two
  that use only those.
- **Approvals are decided on the host's terminal**, as in O-13 (`remote_approvals` is off).
- **One replica, one user.** The sign-in is a single static account.

## Set it up

Check out `ogun` and `zeus-core` side by side (ADR 0002), then, on the host:

```sh
cd ogun/deploy/demo-hub

# 1. Secrets. The same client secret goes to Dex and to ogun-server.
mkdir -p secrets
SECRET=$(openssl rand -base64 32 | tr -d '\n=+/')
printf '%s\n' "$SECRET" > secrets/oidc-client-secret
sudo chown 10001:10001 secrets/oidc-client-secret && sudo chmod 0400 secrets/oidc-client-secret
PASSWORD=$(openssl rand -base64 18 | tr -d '\n=+/')
# Dex wants a bcrypt hash with the $2a$ prefix; htpasswd writes $2y$.
HASH=$(docker run --rm httpd:2.4-alpine htpasswd -bnBC 10 "" "$PASSWORD" | tr -d ':\n' | sed 's/^[$]2y/$2a/')
umask 077
printf "OGUN_CLIENT_SECRET='%s'\nDEMO_PASSWORD_HASH='%s'\n" "$SECRET" "$HASH" > dex/dex.env
printf 'email: demo@ogun.local\npassword: %s\n' "$PASSWORD" > ../../../demo-login.txt

# 2. Build the image (several minutes the first time) and start the daemon.
docker compose build ogund
docker compose up -d ogund

# 3. The bridge credential. Only a person at a terminal can create one.
docker compose exec ogund ogun --profile server credentials add server --kind bridge

# 4. The rest, then the owner enrolment link (works once; delete the file after use).
docker compose up -d
docker compose exec ogun-server cat /var/lib/ogun/ogun-server/owner-enrolment.txt
docker compose exec ogun-server rm /var/lib/ogun/ogun-server/owner-enrolment.txt

# 5. Optional: the two seed workflows, and a backfill that runs for real.
docker compose cp workflows ogund:/var/lib/ogun/seed
docker compose exec ogund ogun --profile server put /var/lib/ogun/seed/hub-backfill.workflow.json
docker compose exec ogund ogun --profile server put /var/lib/ogun/seed/hub-heartbeat.workflow.json
docker compose exec ogund ogun --profile server backfill hub-backfill --from 2023-01 --to 2026-09 --yes
```

`hub-heartbeat` fires every fifteen minutes. Stop it by disabling the workflow in Studio-web (admin), or stop the
hub with `docker compose down` (the state volume stays; `down -v` deletes it).

## Use it

From your machine, forward both ports (keep `localhost` in every address, because the sign-in cookie and the OIDC
redirect are bound to that name):

```sh
ssh -N -L 18443:127.0.0.1:18443 -L 18444:127.0.0.1:18444 user@host
```

- **Browser:** open `http://localhost:18443/enroll/<code>` the first time (the owner link from step 4), then
  `http://localhost:18443/` afterwards. Sign in with the demo account.
- **Desktop Studio:** sign in once in the browser, create an API token in Studio-web, then Settings, Environments,
  "Add environment": URL `http://localhost:18443`, the token. A token acts as an agent, so the desktop link is
  read-only (O-13 section 5.2).
- **Agents:** `claude mcp add --transport http ogun http://localhost:18443/mcp --header "Authorization: Bearer ogt1_..."`.

## What was checked, and what was not

Checked on 2026-10-24 on a Linux host: the image builds from this repository's Dockerfile; `ogund` and
`ogun-server` start and talk over the socket; a browser signs in through Dex with PKCE and gets an owner session;
a token reads and is refused where tokens must be (token and workflow management); a 45-partition backfill of
core operators runs to completion; the daemon restarts and keeps its workflows and runs; the workflow hashes the
container computes equal the ones the Windows CLI computes. Not checked: remote approvals, sync from an edge,
the stream ingest bridge, any code operator, and TLS.
