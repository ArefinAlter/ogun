# The hosted pre-alpha hub

`https://ogun.103.191.50.32.sslip.io` is a real Ogun hub on sample data that anyone may use: the real `ogund` and
`ogun-server` (Studio-web, REST, MCP), Dex for sign-in, a small service for the Studio screens that `ogund` does not
implement yet, and sample file servers. The desktop release (0.0.1a) links to it by default. It is a pre-alpha
showcase, not a production setup; [`deploy/demo-hub`](../demo-hub) is the loopback-only variant it replaced.

## What runs

| Container | Image | Port on the host | What |
|---|---|---|---|
| `ogund` | `ogun-prealpha:local` (from [`deploy/Dockerfile`](../Dockerfile)) | none | `ogund --profile server`; its one listener is a Unix socket in the state volume |
| `ogun-server` | the same | `127.0.0.1:18443` | Studio-web, the REST API, MCP, OIDC sign-in |
| `dex` | `ghcr.io/dexidp/dex:v2.41.1` | `127.0.0.1:18444` (shares `ogun-server`'s network) | sign-in pages under `/dex`, two static accounts |
| `svc` | `ogun-prealpha-svc:local` ([`svc/`](svc)) | `127.0.0.1:18445` | the pre-alpha service under `/svc/` |
| `fixture` | `nginx:1.27-alpine` | none | `files.example.org` and `weather.example.org`, inside the network only |

The host's nginx ([`nginx/ogun-prealpha.conf`](nginx/ogun-prealpha.conf), TLS from Let's Encrypt) routes `/` to
`ogun-server`, `/dex/` to Dex and `/svc/` to the service, with rate limits and request-size limits. Every container
has memory, CPU and process limits, a read-only root, no capabilities (the fixture server keeps the three nginx
needs), `no-new-privileges` and rotated logs.

**No outbound internet for workflows.** `ogund` and the fixture server are only on `core`, an `internal: true`
network: a workflow can reach `files.example.org` and `weather.example.org` (the fixture server's aliases) and
nothing else, whatever a visitor writes in `allowed_domains`. `ogun-server` and `svc` are also on `edge`, only so their
ports can be published; neither fetches anything a visitor names.

## Which screens use what

| Product server (`ogund` + `ogun-server`) | Pre-alpha service (`svc`, SQLite in a volume) |
|---|---|
| Overview, Workflows (store, validate, versions, diff), Runs (start, cancel, events), Partitions, Backfills (plan and start), Schedules (from the stored triggers; pausing needs the admin role), Streams, Edges, Approvals (read), Artifacts, Lineage, sign-in | Agent threads (O-17, proposed), Connections, run defaults, Environments overview, the provider ladder, workflow sources, evidence, storage rules, the archive, secret names |

The service is in `svc/src`: `main.ts` (routes), `rest.ts` (calls to `ogun-server`), `store.ts` (SQLite). Its one
route is `POST /svc/v1/call {method, params}` plus `GET /svc/v1/info`; the methods are listed in `main.ts` and, for
the desktop shell, in `apps/studio/src-tauri/src/hub.rs` (`PREALPHA_METHODS`). It decides nothing about who is
calling: it forwards the caller's session cookie or token to `GET /api/v1/me`. Writes need the editor role and, from
a browser, the session's CSRF token.

**The agent** answers from a script (no model is called), but every tool it calls goes to the real daemon through
`ogun-server` with the service's own API token (`secrets/agent-token`), which acts as an agent and so can never
decide an approval. A card's button (store a workflow, start a backfill, retry) runs with the visitor's own session.

## Visitors

`ogun-server` binds one workspace to one daemon, so a per-visitor sandbox would need a daemon per visitor. The hub
therefore uses **one shared sample account**, shown on the sign-in page (`sample@example.org`, password in
`secrets/sample-password`), with the **editor** role: it can store workflows, start and cancel runs and plan
backfills, but cannot pause workflows, manage members or decide anything. What the service keeps (agent threads and
settings) is per browser (an anonymous `ogun_pa_vid` cookie) or per desktop install, with a quota, and expires after
24 hours. The owner account (`owner@ogun.local`, password in `/opt/ogun-prealpha/owner-login.txt`, mode 0600) is for
the person who runs the hub.

**Approvals stay human-only.** `remote_approvals` is off, so no browser and no token can decide one; approvals are
decided by a person at the host's terminal (`docker compose exec ogund ogun --profile server approve ...` in a real
terminal). The image has **no operator sandbox**, so code operators (`python`, `sql`, `dbt`, `shell`, `polars`) never
run here; their versions (rebalance-plan v2, district-report) stay pending.

## The nightly reset

`/etc/cron.d/ogun-prealpha` runs [`reset.sh`](reset.sh) at 03:00 UTC: it stops the stack, deletes the daemon's run
store, artifacts and code bundles and the service's database, starts the stack and runs [`seed.sh`](seed.sh) (the
sample workflows, a 36-month backfill, a rebalance shortlist, a failed forecast, two versions waiting for approval).
Accounts, the bridge credential and API tokens stay; a token a visitor created stays too (it is an agent with at
most the editor role, which the public account already has).

## Set it up

Check out `ogun` (this repository) on the host, with prebuilt Linux binaries in `prebuilt/amd64/` beside it:

```sh
/opt/ogun-prealpha/
  ogun/              this repository
  prebuilt/amd64/    ogund, ogun, ogun-server, ogun-ingest (deploy/pre-alpha/release/build-linux.sh)
  owner-login.txt    written by setup.sh
```

```sh
cd /opt/ogun-prealpha/ogun/deploy/pre-alpha
./setup.sh                                               # secrets, sample files
docker build -f ../Dockerfile --build-arg BINARIES=prebuilt -t ogun-prealpha:local /opt/ogun-prealpha
docker build -f svc/Dockerfile -t ogun-prealpha-svc:local ../..
docker compose up -d ogund
docker compose exec ogund ogun --profile server credentials add server --kind bridge   # on a real terminal
docker compose up -d
# nginx site and TLS (under the shared lock), then:
python3 tools/bootstrap.py https://ogun.103.191.50.32.sslip.io   # owner, sample account, two tokens
docker compose restart svc
./seed.sh
```

Restart: `docker compose restart` in this directory. Stop: `docker compose down` (volumes stay; `down -v` deletes
them). Logs: `docker compose logs --tail 100 <service>`.
