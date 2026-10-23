# O-15. The `ogund` local API

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-01, O-02, O-04, O-07, O-09, O-11 |
| Decided by | ADR 0002 (product boundary), ADR 0006 (no `zeus-api`), Zeus ADRs 0002, 0007 and 0010 (applied unchanged) |
| Implemented by | `ogun-api` (endpoint, framing, handshake, methods, policy table, client); `ogun-daemon` (`ogund`, the server side) |

## 1. Purpose

`ogund` is Ogun's runtime: it stores workflows and runs, fires triggers, executes operators, seals stream windows, keeps approvals and syncs edges. Everything else is a client of it through this API: the `ogun` CLI, `ogun mcp`, Ogun Studio, the ingest bridge, the server bridge, the language server and the data-platform integrations. This spec is the contract between them.

The API has the shape of Zeus 05 (one local listener, newline-delimited JSON-RPC 2.0, a mutual handshake, client kinds fixed by credentials, one policy table) so that one mental model covers both products. Ogun implements it in its own crate: it does not depend on `zeus-api` (ADR 0006).

## 2. Endpoint and state

1. `ogund` MUST have exactly one listener, the local IPC endpoint. HTTP surfaces (Studio-web, MCP over HTTP, A2A, edge sync over HTTPS, HTTP push ingest) are separate bridge processes that connect to this endpoint as clients (Zeus ADR 0010 pattern).
2. The state directory is `$OGUN_HOME` when set, otherwise `$XDG_STATE_HOME/ogun` (Linux), `~/Library/Application Support/Ogun` (macOS) or `%LOCALAPPDATA%\Ogun` (Windows). A profile `<p>` uses `profiles/<p>` beneath it and has its own daemon.
3. Unix: the endpoint is `<runtime_dir>/ogun/ogund.sock` (`ogund-<p>.sock` for a profile). The directory MUST be owned by the user with mode `0700`, the socket `0600`. When the path would not fit `sun_path`, the daemon uses `<tmp>/ogun-<h>/ogund.sock` with `h` the first 12 hex digits of BLAKE3 over the state directory and profile.
4. Windows: the endpoint is `\\.\pipe\ogund-<h>` with `h` the first 16 hex digits of the same digest. The pipe MUST reject remote clients and MUST be created as the first instance, so a squatter makes the daemon fail to start rather than serve beside it.
5. A daemon that finds a live endpoint MUST exit; a stale Unix socket (nothing answers) is replaced.
6. Files in the state directory: `ipc.secret` (`0600`), `credentials/` (`0700`, one `<name>.token` per local credential, `0600`), `credentials.table` (the daemon's verifiers), `runs.redb` (the run store, O-01), `artifacts/` (a Zeus 13 store instance), `scratch/`, `secrets/` (`0700`), `config.json`, `ogund.lock`.

## 3. Framing

1. Each message is one line of UTF-8 JSON followed by `\n`. A request line longer than 1 MiB is `api.payload_too_large` and the daemon closes the connection; clients accept response lines up to 16 MiB.
2. Messages are JSON-RPC 2.0. Every request has an integer `id`; there are no client notifications. A connection carries one request at a time (clients that need concurrency open more connections).
3. Errors use JSON-RPC codes `-32601` (`api.method_not_found`), `-32602` (`api.invalid_params`), `-32603` (`internal.bug`) and `-32000` for everything else. `error.data` is always the O-00 error envelope; `error.message` repeats its message.

## 4. Handshake and credentials

### 4.1 Handshake

```text
client                                               ogund
  daemon.hello {protocol, client, client_nonce}   ->
                 <-  {protocol, daemon_version, profile, server_nonce, server_proof}
  check server_proof = BLAKE3-keyed(ipc_secret, "ogun-server-v1" || client_nonce)
  daemon.authenticate {credential_id, proof}      ->
                 <-  {kind, name, interactive, methods}
```

1. Nonces are 32 random bytes as 64 lowercase hex digits, fresh per connection.
2. A client MUST verify `server_proof` before it sends anything else. A wrong proof means the endpoint is not this user's daemon; the client closes without authenticating.
3. `auth_key = BLAKE3-derive-key("ogun 2026-10 client auth key v1", token)`; `proof = BLAKE3-keyed(auth_key, "ogun-client-v1" || credential_id || server_nonce || client_nonce)`. The token never crosses the wire. The daemon stores `auth_key`, never the token.
4. Before `daemon.authenticate` succeeds, only the two handshake methods are callable; anything else is `api.unauthenticated`. Three failed authentications close the connection.
5. Comparisons are constant time.

### 4.2 Credentials

A local credential is `ocl_<ULID>`, a kind, a name (`[a-z0-9_-]{1,64}`) and a token `ot1_<64 hex>`. The token file holds one line: `<id> <kind> <name> <token>`. On first start `ogund` creates `ipc.secret` and a `human` credential named `cli`. Further credentials are made with `credentials.add` (section 6), which writes the token file and never returns the token over the wire.

### 4.3 Client kinds

| Kind | Who | Ceiling |
|---|---|---|
| `human` | A person at the CLI or Studio | Everything, but trust-changing methods need an interactive peer (section 4.4). |
| `agent` | MCP, A2A, integrations, scripts | Read, author workflows, start and cancel runs. Never decides an approval, never manages credentials, secrets, approvers or edges. |
| `service` | The ingest bridge, edge sync, the Zeus session bridge | Push stream records, sync, read status and artifacts. |
| `bridge` | `ogun-server` acting for its users | As agent, plus approver proposals, sync and approval decisions with WebAuthn evidence. |

The credential fixes the kind. A client's `client.name` in `daemon.hello` is recorded for audit and is never trusted.

### 4.4 Peers and interactivity

At accept time the daemon inspects the peer:

1. Unix: the peer's uid MUST equal the daemon's effective uid; otherwise the connection is refused.
2. Linux: the daemon reads `/proc/<pid>/stat` (`tty_nr`) and `/proc/<pid>/exe`. A connection is **interactive** when the peer has a controlling terminal and its executable is one of the daemon's pinned binaries: the `ogun` and `ogun-studio` files next to `ogund`, plus any listed under `trusted_binaries` in `config.json`.
3. macOS and Windows: the daemon does not inspect peers in this draft, so no connection is interactive there. Methods that need an interactive peer fail with `api.permission_denied`; approvals on those platforms wait for a later draft that adds platform modules (`LOCAL_PEERPID` and `GetNamedPipeClientProcessId`) or for Studio-web with WebAuthn. This is a known gap, tracked in `docs/IMPLEMENTATION_STATUS.md`.
4. **Honest limit** (as Zeus ADR 0007 point 5): a program that runs as the same user with shell access can read the same credential files and drive a pseudo-terminal. Kind separation, the peer checks, the request hash, the typed confirmation code (O-07) and rate limits narrow this; they do not remove it.

## 5. Methods

Mutating methods that an agent can reach take an `idempotency_key` (1 to 128 printable ASCII characters). A repeat with the same key and the same parameters returns the first result; with different parameters it fails with `api.idempotency_conflict`. Keys are kept for 24 hours.

Strings that came from outside Ogun (descriptions, operator messages, remote errors) are in members named `untrusted_*`.

| Method | Parameters | Result |
|---|---|---|
| `daemon.hello` | `protocol`, `client {name, version}`, `client_nonce` | `protocol`, `daemon_version`, `profile?`, `server_nonce`, `server_proof` |
| `daemon.authenticate` | `credential_id`, `proof` | `kind`, `name`, `interactive`, `methods` |
| `daemon.status` | none | counters: `workflows`, `runs_active`, `approvals_pending`, `streams`, `last_event_seq`, `scheduler_enabled`, `started_at` |
| `daemon.shutdown` | none | `{}`; in-flight attempts checkpoint first |
| `workflows.validate` | `workflow` | `ok`, `workflow_hash?`, `diagnostics` |
| `workflows.put` | `workflow`, `idempotency_key` | `workflow_id`, `name`, `version`, `workflow_hash`, `created`, `diagnostics`, `approvals` |
| `workflows.list` | none | `workflows` |
| `workflows.get` | `name`, `version?` | `summary`, `version`, `workflow_hash`, `workflow` (normal form) |
| `workflows.set_enabled` | `name`, `enabled` | the summary |
| `runs.start` | `workflow`, `version?`, `workflow_hash?`, `partitions`, `idempotency_key` | a run view |
| `runs.get` | `run_id` | a run view with node instances |
| `runs.list` | `workflow?`, `before?`, `limit?` | `runs`, `next?` |
| `runs.cancel` | `run_id`, `idempotency_key` | a run view |
| `backfills.plan` | `workflow`, `from`, `to` | `workflow_hash`, `to_run`, `up_to_date`, `ranges` |
| `partitions.status` | `workflow`, `version?`, `from?`, `to?` | `workflow_hash`, `ranges` |
| `events.since` | `after`, `run_id?`, `limit?`, `wait_ms` | `events`, `last_seq` |
| `approvals.list` | `state?` | `approvals` |
| `approvals.get` | `approval_id` | an approval view |
| `approvals.decide` | `approval_id`, `decision`, `request_hash`, `evidence` | an approval view |
| `artifacts.list` | `run_id` | `artifacts` |
| `artifacts.get` | `blake3`, `inline_max_bytes` | `artifact`, `path`, `content_base64?` |
| `lineage.get` | `run_id?` or `blake3?` | `events` (OpenLineage run events, O-05) |
| `streams.push` | `workflow`, `node`, `first_seq`, `records` | `accepted_through`, `dropped`, `late`, `retry_after_ms` |
| `streams.list` | none | `streams` |
| `streams.status` | `workflow`, `node` | a stream view |
| `credentials.add` | `name`, `kind` | `credential`, `token_file` |
| `credentials.list` | none | `credentials` (never tokens) |
| `credentials.revoke` | `credential_id` | the credential |
| `secrets.put` | `name`, `value` | `{}` |
| `secrets.list` | none | `names` |
| `secrets.delete` | `name` | `{}` |
| `approvers.propose` | `credential_id`, `public_key`, `user`, `rp_id` | an approver view |
| `approvers.list` | none | `approvers` |
| `approvers.confirm` | `credential_id`, `fingerprint` | an approver view |
| `approvers.revoke` | `credential_id` | an approver view |
| `edges.enroll` | `name`, `public_key` | an edge view |
| `edges.list` | none | `edges` |
| `sync.offer` | `edge_id`, `offer`, `signature` | `want` |
| `sync.chunk` | `edge_id`, `partition`, `blake3`, `index`, `total`, `data_base64` | `complete` |

Partition selectors for `runs.start`: `{"kind": "latest"}`, `{"kind": "keys", "keys": [...]}` or `{"kind": "range", "from": "...", "to": "...", "force": false}`. A range skips keys already done for the workflow hash unless `force` is true (O-04 section 6).

`partitions.status` and `backfills.plan` compress consecutive keys with the same state (`done`, `running`, `failed`, `missing`) into `{from, to, count, state}` ranges, so a year of five-minute windows is a handful of rows, not 105 120.

`streams.push`: records are `{event_time_ms, key, payload}`, at most 10 000 per call. `first_seq` is the producer's sequence number of the first record; the daemon drops records at or below the last `accepted_through` it returned for that producer, so a producer that retries after a lost response does not duplicate. `accepted_through` means durable in the daemon's ingest log: the producer may forget those records.

The exact member types are the Rust types in `ogun_api::methods`; they are the schema for this draft.

## 6. Policy

`ogun_api::policy::METHODS` is the single table of which kinds may call which method, and which methods need an interactive peer. A method missing from the table is `api.method_not_found` for everyone. The table, in summary:

| Methods | Kinds |
|---|---|
| `daemon.status`, `workflows.validate/list/get`, `runs.get/list`, `partitions.status`, `events.since`, `artifacts.list/get`, `streams.list/status`, `edges.list` | all (services read artifacts for edge sync, O-11) |
| `workflows.put`, `runs.start/cancel`, `backfills.plan`, `approvals.list/get`, `lineage.get` | human, agent, bridge |
| `workflows.set_enabled`, `approvers.list` | human, bridge |
| `approvals.decide` | human (with `terminal` or `studio` evidence from an interactive peer), bridge (with `webauthn` evidence) |
| `streams.push` | service |
| `sync.offer`, `sync.chunk` | service, bridge |
| `approvers.propose` | bridge |
| `credentials.list`, `secrets.list` | human |
| `daemon.shutdown`, `credentials.add/revoke`, `secrets.put/delete`, `approvers.confirm/revoke`, `edges.enroll` | human, interactive peer only |

Rules that the table alone does not express:

1. `approvals.decide` with `terminal` evidence: the connection is `human` and interactive, and the peer executable is the pinned `ogun` binary. With `studio` evidence: `human`, and the peer executable is the pinned `ogun-studio` binary. With `webauthn` evidence: the connection is `bridge`, the approver key is confirmed (section 7) and the assertion verifies. Any other combination is `approval.channel_required`. Agents and services never reach a decision, whatever evidence they send (constraint 3).
2. A decision names the approval's `request_hash`; a mismatch is `approval.hash_mismatch`. A decided approval cannot be decided again. At most 50 approvals are pending and at most 10 decisions are accepted per minute (Zeus ADR 0007).
3. `secrets.put` values are zeroized after they are written; secrets never appear in events, logs, errors or lineage (constraint 4). `secrets.list` returns names only.

## 7. WebAuthn approvers

A self-hosted server (O-13) relays decisions from Studio-web. The daemon never trusts the bridge's word for a person: each decision carries a WebAuthn assertion that the daemon verifies itself.

1. The server registers a passkey for one of its users and calls `approvers.propose` with the ES256 public key (uncompressed SEC1 P-256, base64url), the WebAuthn credential id, the user and the relying party id. The daemon stores it unconfirmed and returns its `fingerprint` (the first 16 hex digits of BLAKE3 over the key).
2. A person on the host's terminal runs `ogun approvers confirm <credential_id> <fingerprint>`, reading the fingerprint from the server's page. Only then may the key sign decisions. A compromised bridge can propose keys but cannot confirm them.
3. The assertion's challenge MUST be `BLAKE3(approval_id || request_hash || decision)` in base64url. The daemon checks `clientDataJSON.type == "webauthn.get"`, the challenge, `rpIdHash == SHA-256(rp_id)`, the user-present and user-verified flags, and the ES256 signature over `authenticatorData || SHA-256(clientDataJSON)`.
4. ADR 0010: remote approvals need an external security review before release; until then a build exposes them only behind the `server.remote_approvals` configuration flag, off by default.

## 8. Events

`events.since` returns store events (O-01 section 6) in sequence order. With `wait_ms > 0` and nothing after `after`, the daemon holds the call until an event arrives or the wait elapses (at most 30 000 ms). Events carry ids, states and causes, never configuration values or secrets.

## 9. Conformance

`cargo test -p ogun-api` checks the handshake (round trip, replay with another nonce, wrong secret), credential file round trips (and that `Debug` never prints a token), the policy table (unique methods; agents and services can never call decide, credential, secret, approver-confirm, edge-enrol or shutdown methods; interactive rows are human-only) and the Linux `tty_nr` parser. `cargo test -p ogun-daemon` runs the daemon on a temporary state directory and drives it through the client.
