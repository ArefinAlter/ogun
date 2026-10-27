# Implementation status

What runs today, what is only built, and the known gaps. Updated with every runtime change. The specs are the contract; this file says how far the code is from it. "Proven" means a test in this repository exercises it; "built" means the code compiles and has unit tests but has not run against the real thing it talks to.

## Proven end to end (2026-10-23, Linux)

| What | Test |
|---|---|
| The O1 exit test: a 121-partition backfill survives `kill -9` of `ogund`; finished partitions are not run again, the rest finish after the restart, and a second backfill has nothing to do | `crates/ogun-daemon/tests/exit.rs` |
| The stream twin: records pushed into an open window survive `kill -9`; a retried push is not ingested twice; the window seals with exactly those records | `crates/ogun-daemon/tests/exit.rs` |
| The O-15 handshake, client kinds and the policy table over a real connection: agents cannot decide approvals or read secrets; a human connection without a terminal cannot change trust | `crates/ogun-daemon/tests/daemon.rs` |
| A stream push sealed into a window partition that starts a run | `crates/ogun-daemon/tests/daemon.rs` |
| Hub sync: an enrolled edge's signed offer, a three-chunk upload, BLAKE3 verification and import; a forged signature refused | `crates/ogun-daemon/tests/daemon.rs` |
| Core operators and the `jsonl-stdio/0` runner under bubblewrap, including the Smart Air python operators unchanged, cancellation and secret scrubbing of stderr | `crates/ogun-ops/tests` |
| Model operators against a mock provider on loopback; the spend ledger's hash chain | `crates/ogun-llm/tests`, `crates/ogun-op-llm/tests` |
| `ogun-server` against a fake `ogund`: OIDC, sessions, CSRF, roles, API tokens acting as agents, the passkey registration and a WebAuthn decision relayed to the daemon | `crates/ogun-server/tests` |
| The language server over stdio; the VS Code client against the built `ogun-lsp` | `crates/ogun-lsp`, `extensions/vscode/test` |
| Lineage events from the run store; a local benchmark matrix with network denied; the Zeus bridge against a fake `zeusd` with import into a real artifact store | `crates/ogun-lineage`, `crates/ogun-bench`, `crates/ogun-zeus-bridge` |

## Proven end to end on Windows with the standalone Studio (2026-10-27)

Run against the real installed app (NSIS build, scratch profile) by `tools/acceptance`; how, the stand-ins used and every
limit are in [`ACCEPTANCE-0.0.1b.md`](ACCEPTANCE-0.0.1b.md).

| What | Scenario |
|---|---|
| Studio starts its own `ogund` (a named pipe, no TCP listener, no secret on the command line), restarts it when it dies and stops it with the app; the first screen imports a sample project; the stored workflow hashes equal `ogun hash` of the files | 1 |
| `acquire`, `parse`, `filter`, `partition`, `hash` and `manifest` on a local fixture and on the real GitHub and USGS APIs; every artifact's bytes hash to its digest; OpenLineage events | 2 |
| A 121-partition backfill survives killing `ogund` (the app restarts it; partitions done before are not fetched again) and killing the whole app | 2 |
| A cron schedule fires by itself while the app is open and stops when paused; a file-tail stream seals event-time windows, revises one for a late record, ignores a duplicate and survives a restart of the ingest bridge | 3 |
| Approvals in Studio: daemon facts before the buttons, an arming delay, the request hash and the Studio channel, a spent confirmation, no relay of `approvals.decide` from the webview, no credential for another program | 4 |
| A Python code operator (the Python 3.13 the installer ships) runs only after its hash was approved, inside a Windows AppContainer: no profile folder, no write outside scratch, no ungranted file, no loopback | 5 |
| Agent threads: consent, the closed tool table at the thread's access level, run cards, spend caps, the offline switch, secrets refused in instructions and messages, an invented `approvals.decide` refused (against a local server speaking the OpenAI wire format) | 6 |
| Sign-in to a hub from Studio with the browser (authorization code, PKCE, loopback redirect), tokens in the Windows credential store, refresh, sign-out, roles, agents read-only for approvals (a real `ogun-server` and `ogund`, an OpenID Connect stand-in for Dex) | 7 |
| Connections made in the editor with secrets by name, a Test that sends real requests, exports to a folder, SQLite and an S3-compatible store that checks the signature | 8 |
| Settings persist across restarts; the evidence bundle downloads from a run page; storage rules give a plan and a cleanup that removes real bytes | 9 |
| The sample benchmark suite from the app on the local runtime in an AppContainer with the network denied, hash-chained trajectories | 10 |

## Built, not yet run against the real counterpart

- **`ogun-ingest`** against a real MQTT broker, LoRaWAN network server or Modbus device (unit and loopback HTTP tests only).
- **`ogun-sync`** edge client against a live hub over HTTPS (the hub side is proven above).
- **`ogun-server`** against the real `ogund`, in a container built from `deploy/Dockerfile`, with a real OIDC provider (Dex): run by hand on a Linux host on 2026-10-24 through `deploy/demo-hub` and `tools/ui-reference/hub_check.py`, not by a test in CI. The image builds; a browser signs in with PKCE and gets an owner session; Studio-web loads every page against the daemon with no console error; an API token reads and is refused where tokens must be; a scheduled run fires on its cron minute; a 45-partition backfill of core operators completes and survives a daemon restart. Not run: a production identity provider, TLS, remote approvals, an edge syncing to it.
- **The hosted pre-alpha hub** (`deploy/pre-alpha`, 2026-10-25): the same image behind nginx with TLS at `https://ogun.103.191.50.32.sslip.io`, Dex sign-in with a shared sample account (editor), sample workflows fetching from fixture servers on an internal network, and the pre-alpha service (`deploy/pre-alpha/svc`) for agent threads and the settings `ogund` does not implement. Checked with `tools/ui-reference/hub_check.py --svc`: sign-in, every Studio-web page, an agent turn whose tool calls reach the daemon, a stored connection. Not run: per-visitor isolation on the daemon (one workspace per daemon; the service isolates only its own data), remote approvals.
- **Studio desktop on Linux and macOS.** Windows is proven above. The Linux AppImage and .deb were built and the AppImage was started headless (Xvfb, a scratch home): the window, its own `ogund` and `ogun-ingest`, and the engine stopping with the app; no scenario was driven through it. macOS is not built. **Android** and **iOS** builds (no SDK, NDK or Xcode here; the Kotlin gateway service has not been compiled).
- **Agent threads** (O-17, ADR 0014, accepted by the maintainer's instruction of 2026-10-26) are implemented in `ogund` and run against local servers that speak the OpenAI chat-completions wire format (streamed deltas, tool-call deltas, usage). The Anthropic, Gemini and Ollama adapters are checked against mock servers in `crates/ogun-daemon/tests/agent.rs` only. **No live provider was called**: no key was available. The scripted agent of the hosted pre-alpha hub is unchanged.
- **The Windows window frame**: the shell's main window has no native title bar; the page draws minimize, maximize and close. Pressed with real mouse input (`tools/ui-reference/chrome_check.py`, 12 checks: maximize and restore, move, double click, resize, minimize, close) on Windows 11. The macOS overlay title bar and the Linux frame are configured or left alone but not run.
- **Studio Settings and the connector catalog** (O-16, `apps/studio/web/src/views/settings`, `connectors/`): the daemon methods now exist (`connections.*`, `runtime.*`, `environments.list`, `storage.*`, `evidence.*`, `ladder.*`, `sandbox.status`, `bench.*`) and are exercised by the acceptance run. Of 101 connectors, 31 are `built` and 70 are manifests only; only `local-files`, `sqlite`, `minio` and `s3-compatible` deliveries and Tests ran, the S3 ones against a local server written from the public SigV4 documentation, never against a real S3 or MinIO service. The 15 `ogun-export` sinks pass 202 unit tests against local fakes.
- **Model providers**: every adapter follows the provider's public API and was checked only against local mock servers; none has been called for real.
- **Benchmark adapters** (Harbor, Spider 2.0-DBT, ADE-bench): their task layouts are written from public descriptions and marked unverified in O-06; the docker/podman environment has not run, and Studio offers only the sample suite (scripted agents) on the local runtime.
- **Data-platform integrations**: Dagster and Prefect smoke-tested against a fake daemon; Airflow only against stub modules.
- **The Zeus session bridge** against a real `zeusd`.
- **Desktop sign-in to the hosted pre-alpha hub.** The hub still runs the 0.0.1a backend; browser sign-in from Studio desktop needs `POST /auth/native` and a public Dex client, which `deploy/pre-alpha/native-signin` adds (not run: no access to the host). It ran against a local hub with an OpenID Connect provider that behaves like Dex for a public client, not against Dex itself.

## Known gaps

1. **Peer inspection exists on Linux and Windows, not on macOS.** On Windows the pinned executable is identified by the image path of the process at the other end of the named pipe; the terminal surface is decided by that path alone (there is no tty check). On macOS no connection is interactive, so terminal and Studio approvals and the trust-changing methods (credentials, secrets, approver confirmation, edge enrolment, shutdown) are refused there (O-15 section 4.4).
2. **The Windows operator sandbox has limits** ([`windows-sandbox.md`](windows-sandbox.md), ADR 0017): file names stay visible in the folders that hold an operator's inputs, files every app package may read stay readable, network access is all or nothing, there is no disk quota, `powershell.exe` does not start inside it, and a program folder installed for all users outside Program Files is copied once (up to 1.5 GiB). macOS has no sandbox in this release.
3. **Budgets**: only model spend is enforced (per run, from usage events); `bytes`, `files` and `wall_ms` reservations are not applied by the executor.
4. **Manifests with a host allowlist** are refused (`op.sandbox_unavailable`); model operators share the network rather than being limited to the ladder's hosts.
5. **Partition status** scans the 2 000 most recent runs for running and failed partitions; done partitions come from the store's index and are exact.
6. **The `late` output** of a stream is a JSONL file beside the source's log, not yet an artifact.
7. **Edge acknowledgements** reach the edge's daemon through `sync/acked.jsonl`, not a daemon method; a deleted bridge log restarts its producer sequence at 0 and needs a producer epoch.
8. **Remote approvals** (Studio-web with WebAuthn) are implemented behind `remote_approvals`, off by default, until the external security review ADR 0010 requires.
9. **The relayed caller**: the daemon sees every call from `ogun-server` as the bridge credential; an `acting_for` member would put the person in the daemon's log (O-13 open issue 1).
10. **CI does not run**: GitHub Actions jobs do not start on this account (billing).
11. **The container image has no operator sandbox.** `deploy/Dockerfile` installs neither bubblewrap nor Python (checked in a built container), so a workflow with a code operator cannot run in the shipped image. The compose files also drop every capability and keep Docker's default seccomp profile, under which bubblewrap's namespaces are normally refused; that part was not tested. Core operators run. The self-hosting guide does not say so yet.
12. **Studio-web read `GET /api/v1/me` as a flat object** while `ogun-server` returns the user record and the memberships, so a signed-in owner appeared as "signed in, no role" and could not write. Found by the 2026-10-24 hub run; fixed in the frontend (`apps/studio/web/src/api/identity.ts`, tested by `identity.test.mjs`). A person who belongs to several workspaces still needs the `x-ogun-workspace` header, which Studio-web does not send.
13. **A run's Events tab in Studio-web shows only events that arrive while the page is open**; it does not replay a finished run's history.
14. **The transport sometimes reports `http.protocol_error` for the first request to a public API** (seen against `api.github.com` on Windows). `acquire` maps it to a retryable error so the node's retry policy tries again; the cause is in the zeus-core transport and is not fixed here.
15. **`export` needs a human approval for every delivery** (the validator requires `policy.approval: always`), so an unattended export is not possible.
16. **Benchmarks in Studio** offer only the sample suite; model credentials cannot be passed into a sandboxed local run, and the Harbor, Spider 2.0-DBT and ADE-bench adapters are not offered there.
17. **A stream workflow stored in Studio starts at the next refresh of the ingest bridge** (up to 30 seconds), and a window seals only when every active key's watermark has passed it (O-09): a quiet station holds the others back until its idle timeout.
