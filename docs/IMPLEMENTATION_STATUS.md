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

## Built, not yet run against the real counterpart

- **`ogun-ingest`** against a real MQTT broker, LoRaWAN network server or Modbus device (unit and loopback HTTP tests only).
- **`ogun-sync`** edge client against a live hub over HTTPS (the hub side is proven above).
- **`ogun-server`** against the real `ogund`, in a container built from `deploy/Dockerfile`, with a real OIDC provider (Dex): run by hand on a Linux host on 2026-10-24 through `deploy/demo-hub` and `tools/ui-reference/hub_check.py`, not by a test in CI. The image builds; a browser signs in with PKCE and gets an owner session; Studio-web loads every page against the daemon with no console error; an API token reads and is refused where tokens must be; a scheduled run fires on its cron minute; a 45-partition backfill of core operators completes and survives a daemon restart. Not run: a production identity provider, TLS, remote approvals, an edge syncing to it.
- **Studio desktop** linked to that hub with an API token (read-only) was run by hand on Windows on 2026-10-24; Studio desktop against a local `ogund` has not run. **Android** and **iOS** builds (no SDK, NDK or Xcode here; the Kotlin gateway service has not been compiled).
- **The agent screen** (O-17, ADR 0014, both Proposed): the conversation interface, the closed tool table, access levels, cards and the Settings page are built and exercised on a scripted agent (`apps/studio/web/src/demo/agent.ts`, 52 checks in `tools/ui-reference/smoke_agent.py`, plus `npm test` for the tool table and the Markdown reader). **No daemon implements `agent.*`**, no model is called anywhere, and the screen is hidden on a daemon without it (it shows the MCP commands that work now).
- **The Windows window frame**: the shell's main window has no native title bar; the page draws minimize, maximize and close. Pressed with real mouse input (`tools/ui-reference/chrome_check.py`, 12 checks: maximize and restore, move, double click, resize, minimize, close) on Windows 11. The macOS overlay title bar and the Linux frame are configured or left alone but not run.
- **Studio Settings and the connector catalog** (O-16, `apps/studio/web/src/views/settings`, `connectors/`): the pages are exercised end to end against the scripted demo backend (`tools/ui-reference/smoke_studio.py`, 59 checks, also under a frozen `Object.prototype`) and render against the real hub with the unsupported ones read-only. The daemon methods they would call (`connections.*`, `runtime.*`, `environments.list`, `storage.rules`, `sources.*`) do not exist; O-16 proposes them. Of 99 connectors, 29 are `built` and 70 are manifests only; none has run against its real service. The 15 `ogun-export` sinks pass 202 unit tests against local fakes written from public documentation.
- **Model providers**: only the Anthropic client was checked, against a mock; OpenAI, Google, Ollama and OpenAI-compatible clients follow the public APIs but have not been called.
- **Benchmark adapters** (Harbor, Spider 2.0-DBT, ADE-bench): their task layouts are written from public descriptions and marked unverified in O-06; the docker/podman environment has not run.
- **Data-platform integrations**: Dagster and Prefect smoke-tested against a fake daemon; Airflow only against stub modules.
- **The Zeus session bridge** against a real `zeusd`.

## Known gaps

1. **Peer inspection exists on Linux only.** On macOS and Windows no connection is interactive, so terminal and Studio approvals and the trust-changing methods (credentials, secrets, approver confirmation, edge enrolment, shutdown) are refused there (O-15 section 4.4). Studio-web with WebAuthn is the alternative once remote approvals pass their review.
2. **Windows has no operator sandbox**: child operators are refused unless `allow_unsandboxed` is set.
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
