# Studio desktop sign-in on the hosted hub (0.0.1b)

Ogun Studio 0.0.1b signs a person in to a hub with their browser (authorization code with PKCE and a loopback
redirect), then trades the ID token for the hub's own session (`POST /auth/native`, O-13 section 4.3). The hosted
pre-alpha hub needs three changes for that. **They are not applied by the release**: the hub keeps running 0.0.1a
until the owner runs `apply.sh` on the host, because they restart the hub's containers.

| Change | Where | What |
|---|---|---|
| A public Dex client | `dex/config.yaml` | `ogun-studio`, `public: true`; Dex accepts a loopback redirect on any port for a public client, so no port is listed |
| A native client on the provider | `server.json` | `"native_client_id": "ogun-studio"` on the `sample` provider |
| The 0.0.1b binaries | `ogun-prealpha:local` image | `ogun-server` with `POST /auth/native`, built from the release's Linux archive |

Nothing else changes: the nginx site, the TLS certificate, the service, the nightly reset and the seeded sample
stay as they are. Browser sign-in to Studio-web keeps using the `ogun` client.

## Apply

On the host, as the user that runs the stack, with the release archive `ogun-0.0.1b-linux-x64.tar.gz` downloaded
from <https://github.com/ArefinAlter/ogun/releases/tag/v0.0.1b>:

```sh
cd /opt/ogun-prealpha/ogun/deploy/pre-alpha/native-signin
./apply.sh /path/to/ogun-0.0.1b-linux-x64.tar.gz
./check.sh https://ogun.103.191.50.32.sslip.io
```

`apply.sh` backs up `dex/config.yaml`, `server.json` and the current image (tagged `ogun-prealpha:0.0.1a`) under
`/opt/ogun-prealpha/backups/native-signin-<time>/`, edits the two files idempotently, builds the new image from the
archive's binaries, and restarts `ogund`, `dex` and `ogun-server` in that order. It does not touch nginx, the
certificate, `svc` or the volumes.

## Roll back

```sh
./rollback.sh /opt/ogun-prealpha/backups/native-signin-<time>
```

restores both files, points `ogun-prealpha:local` back at the saved image and restarts the same three containers.

## What it does not do

It does not add accounts. A person who signs in with the browser needs an account on the hub: the sample account
(`sample@example.org`, editor) and the owner already have one. An identity without an account is refused with a
message that says to ask for an invitation.

## Status

These scripts were written for the 0.0.1b release and **have not been run against the hosted hub**: the build agent had
no access to the host. The same sign-in was run end to end against a local `ogun-server`, a local `ogund` and a
local OpenID Connect provider that behaves like Dex for a public client (see `docs/ACCEPTANCE-0.0.1b.md`), not
against Dex itself.
