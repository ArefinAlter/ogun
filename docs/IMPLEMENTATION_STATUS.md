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
- **`ogun-server`** against the real `ogund` (proven against a fake one), in a container (the Dockerfile was not built), and with a real OIDC provider.
- **Studio desktop** against the real `ogund`; **Android** and **iOS** builds (no SDK, NDK or Xcode here; the Kotlin gateway service has not been compiled).
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
