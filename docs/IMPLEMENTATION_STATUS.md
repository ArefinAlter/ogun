# Implementation status

What runs today, what is partial, and the known gaps. Updated with every runtime change. The specs are the contract; this file says how far the code is from it.

## Runs today (2026-10-23)

| Area | State |
|---|---|
| `ogund` (`crates/ogun-daemon`) | One local listener (Unix socket in a `0700` directory; named pipe on Windows), the O-15 handshake, the policy table, workflows (`put` with code bundles and approvals, `list`, `get`, `validate`, `set_enabled`), runs (`start` with idempotency, `get`, `list`, `cancel`), backfill plans, range-compressed partition status, long-polling events, approvals with terminal, Studio and WebAuthn evidence, credentials, secrets, approver keys, edge enrolment, artifacts and a first lineage view. The executor drives the O-01 state machine (readiness, routing by the closed predicate set, retries with back-off, cancellation, `approval: always`, model-budget cut-off) and commits outputs to a Zeus 13 artifact store. The scheduler fires cron and interval triggers. Streams: durable per-source ingest logs, windows, deterministic sealing into partitions that start runs, crash replay at the original ingest times, producer-sequence dedupe. |
| `ogun` CLI | Offline: `validate`, `hash`, `diff`, `partitions`, `next`, `hash-file`, `mcp`. Daemon: `daemon start/status/stop/paths`, `put`, `workflows`, `run` (`--wait`), `backfill`, `partition-status`, `runs`, `show`, `cancel`, `approvals`, `approve`, `deny`, `credentials`, `secrets`, `approvers`, `edges`, `events`, `artifacts`, `artifact`. |
| Operators | Native core operators, bounded HTTP acquisition/crawling, installed manifests, the sandboxed `jsonl-stdio/0` runner and external tool shims. Children require installed tools and an available sandbox, or the explicit development override. |
| Server | Separate HTTPS bridge with OIDC sign-in, workspace roles, API tokens, REST/events, MCP and edge sync relay. Remote approvals default off; the required external security review is still open. Deployment assets are in `deploy/`. |
| Tests | `cargo test --workspace`, including `ogun-daemon/tests/daemon.rs`: a real daemon on a temporary directory, the run lifecycle, policy refusals over a real connection, and a stream push that seals a window into a run. |

## Being built in parallel (branches `claude/tender-mccarthy-yc1zxo-*`)

The LLM ladder and model operators (`ogun-llm`), the ingest bridge and edge sync (`ogun-ingest`, `ogun-sync`), Studio (desktop, web, mobile), the language server and integrations, and lineage, benchmarks and the Zeus session bridge (`ogun-lineage`, `ogun-bench`, `ogun-zeus-bridge`).

## Known gaps

1. Child operators have no Windows sandbox yet and require `allow_unsandboxed` there; tool availability and installed manifests determine which adapters can run.
2. `sync.offer` and `sync.chunk` answer `api.unavailable` until `ogun-sync` is linked.
3. `WaitingForSession` instances wait; the Zeus session bridge is not linked yet.
4. Peer inspection exists on Linux only. On macOS and Windows no connection is interactive, so terminal approvals and the trust-changing methods (credentials, secrets, approver confirmation, edge enrolment, shutdown) are unavailable there (O-15 section 4.4).
5. Budgets: only the model-spend budget is enforced (per run, from operator usage events); `bytes`, `files` and `wall_ms` reservations are not yet applied by the executor.
6. Partition status scans the 2 000 most recent runs for running and failed partitions; done partitions come from the store index and are exact.
7. The `late` output of a stream is a JSONL file beside the source's log, not yet an artifact.
8. Lineage is a first view (one event per finished node instance); O-05 and `ogun-lineage` replace it.
9. CI does not run: GitHub Actions jobs do not start on this account (billing).
