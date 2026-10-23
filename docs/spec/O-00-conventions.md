# O-00. Conventions delta

| | |
|---|---|
| Status | Draft 1 |
| Depends on | Zeus spec 00 (conventions) |
| Decided by | ADR 0002 decision 10; ADR 0006 decision 4 |
| Implemented by | `ogun-types` |

## 1. Purpose

Ogun adopts Zeus spec 00 by reference. This document lists only what Ogun adds or narrows: its identifier prefixes, its error codes and its local API transport.

## 2. Inherited unchanged

Everything in Zeus 00 sections 3 to 10 applies to Ogun as written, including:

1. JSON on the wire: `snake_case` members, enums as `snake_case` strings, no floating-point numbers in anything hashed.
2. Instants: RFC 3339, UTC, exactly three fractional digits. Durations: integer milliseconds, suffix `_ms`. Sizes: integer bytes, suffix `_bytes`. Money: integer micro-dollars, suffix `_usd_micro`.
3. Canonical JSON and hashing: RFC 8785 (JCS) and BLAKE3, with the rules of Zeus 00 section 7. Digests are 64 lowercase hex characters in a member named for the algorithm.
4. Redaction: URLs reach logs, events, lineage and errors only through `redact_url`. Secrets use zeroizing types and never implement `Serialize`. Leak tests use the marker `ZEUS-CANARY-`.
5. The error envelope and its classes (Zeus 00 section 9).

## 3. Identifiers

Ogun identifiers are prefixed ULIDs with exactly the syntax of Zeus 00 section 5.1. Only `ogund` mints them. The prefixes below are Ogun's; none equals a Zeus prefix. Zeus 00 section 5.1 lists them through a Zeus pull request; until it merges, this table is the reservation.

| Prefix | Entity | Persisted | Notes |
|---|---|---|---|
| `wf_` | Workflow | yes | All versions of one workflow name. A version is named by its workflow hash (O-02 section 8). |
| `orun_` | Run | yes | One execution of one workflow version. Zeus's `run_` is a Zeus workflow run. |
| `ni_` | Node instance | yes | One node of one run for one partition. |
| `oap_` | Approval | yes | An Ogun approval. Zeus approvals keep `ap_`. |
| `ast_` | Asset | yes | A logical object with versions. |
| `env_` | Environment | yes | The isolated place a run executes. |
| `evl_` | Evaluator | yes | Something that scores a run. |
| `agr_` | Agent run | yes | A recorded agent run. |
| `edg_` | Edge | yes | An edge installation; the namespace of the partitions it writes (O-11). |
| `syn_` | Sync session | yes | One edge-to-hub sync session (O-11). |
| `ocl_` | Local credential | yes | A client credential of `ogund` (O-15 section 4). Zeus's local credentials keep `cli_`. |
| `usr_` | Server user | yes | A user of a self-hosted Ogun server (O-13). |
| `wsp_` | Server workspace | yes | A workspace on a self-hosted Ogun server (O-13). |
| `tok_` | API token | yes | An Ogun server API token; always acts as an agent, never as a person (O-13). |

Artifacts use `art_` from the `zeus-artifact` store (Zeus 13): Ogun runs its own store instance through that crate, and the id is local to the store, while the BLAKE3 digest is the identity across stores.

## 4. Local API transport

`ogund` has one listener, a local IPC endpoint (Unix domain socket, or a named pipe on Windows), speaking JSON-RPC 2.0 as Zeus 05 does. HTTP surfaces (MCP over HTTP, A2A) are separate bridge processes, off by default (Zeus ADR 0010 pattern). The method set, handshake and policy table are O-15.

## 5. Errors

### 5.1 Envelope

Errors use the Zeus 00 section 9.1 envelope unchanged (`code`, `class`, `message`, `retryable`, optional `details`). Messages never contain secrets, URLs or values copied from a workflow's configuration.

### 5.2 Codes

| Code | Class | Retryable | Raised when |
|---|---|---|---|
| `ir.invalid` | `invalid_request` | no | A workflow has error diagnostics (O-02 section 10); `details.codes` lists them. |
| `ir.too_large` | `invalid_request` | no | A workflow document exceeds 1 MiB. |
| `ir.version_unsupported` | `unsupported` | no | `ir_version` is not one this build reads. |
| `ir.hash_mismatch` | `conflict` | no | A request names a workflow hash other than the stored version's. |
| `run.invalid_transition` | `conflict` | no | A node instance transition is not in the O-01 table, or the actor may not cause it. |
| `run.not_found` | `not_found` | no | No such run or node instance. |
| `run.approval_required` | `policy_denied` | no | The request needs a human approval that does not exist yet. |
| `run.budget_exhausted` | `policy_denied` | no | A reservation would exceed a budget and no approval allows it. |
| `op.unknown` | `unsupported` | no | No installed operator has this name and major version. |
| `op.sandbox_violation` | `policy_denied` | no | An operator tried something its manifest does not declare. |
| `op.crashed` | `internal` | no | An operator process exited without a result. |
| `op.timeout` | `timeout` | yes | An attempt exceeded `policy.timeout_ms`. |
| `internal.bug` | `internal` | no | An invariant was violated. |
| `api.unauthenticated` | `unauthenticated` | no | The handshake failed, or a call came before it (O-15). |
| `api.permission_denied` | `policy_denied` | no | The connection's kind or peer may not call the method (O-15 section 6). |
| `api.method_not_found` | `unsupported` | no | No such method. |
| `api.invalid_params` | `invalid_request` | no | Parameters do not match the method's shape. |
| `api.payload_too_large` | `invalid_request` | no | A request line exceeds 1 MiB. |
| `api.unavailable` | `unavailable` | yes | `ogund` is not running, is shutting down or cannot reach its store. |
| `api.idempotency_conflict` | `conflict` | no | An idempotency key was reused with different parameters. |
| `workflow.not_found` | `not_found` | no | No workflow or version by that name. |
| `workflow.not_approved` | `policy_denied` | no | The version needs approvals that are not granted (O-07). |
| `approval.not_found` | `not_found` | no | No such approval. |
| `approval.hash_mismatch` | `conflict` | no | A decision named a request hash other than the approval's. |
| `approval.channel_required` | `policy_denied` | no | The decision lacks evidence the daemon can check for this connection. |
| `approval.already_decided` | `conflict` | no | The approval is no longer pending. |
| `approval.expired` | `policy_denied` | no | The approval expired; it counts as a denial. |
| `artifact.not_found` | `not_found` | no | No artifact with that digest. |
| `stream.unknown` | `not_found` | no | No stream source node by that name. |
| `stream.full` | `unavailable` | yes | The stream buffer is full under `when_full: block`; retry after `retry_after_ms`. |
| `credential.not_found` | `not_found` | no | No such local credential. |
| `credential.name_taken` | `conflict` | no | A live credential already has that name. |
| `secret.not_found` | `not_found` | no | A `secret_ref` names no stored secret. |
| `edge.not_enrolled` | `policy_denied` | no | Sync from an edge the hub has not enrolled. |
| `sync.bad_signature` | `policy_denied` | no | A sync message's Ed25519 signature does not verify. |
| `sync.digest_mismatch` | `conflict` | no | Received bytes do not hash to the offered digest. |
| `llm.not_configured` | `unavailable` | no | No provider in the ladder can serve a model operator (O-03 section 8.10). |
| `llm.config_invalid` | `invalid_request` | no | `llm.json` or a model operator's configuration is invalid. |
| `llm.provider_unavailable` | `unavailable` | yes | Every provider tried failed transiently (429, 5xx, timeout). |
| `llm.auth_failed` | `unauthenticated` | no | A model provider rejected its API key. |
| `llm.request_rejected` | `invalid_request` | no | A model provider rejected the request as invalid. |
| `llm.refused` | `policy_denied` | no | A model or provider declined on policy grounds. |
| `llm.output_invalid` | `integrity` | no | A model's answer failed schema validation after the repair request. |
| `llm.call_limit` | `policy_denied` | no | A model operator's `max_calls` would be exceeded. |
| `llm.input_unsupported` | `invalid_request` | no | A model operator's input is not text, too large or the wrong shape. |
| `lineage.io_failed` | `internal` | no | A lineage export file cannot be read or written (O-05). |
| `lineage.endpoint_unsupported` | `invalid_request` | no | The configured OpenLineage endpoint is not an `http://` URL without user information (O-05). |
| `lineage.endpoint_failed` | `unavailable` | yes | The OpenLineage endpoint could not be reached, timed out or refused an event (O-05). |
| `lineage.trajectory_broken` | `conflict` | no | A trajectory has a sequence gap or a broken BLAKE3 chain (O-05). |
| `bench.task_invalid` | `invalid_request` | no | A task directory does not match its adapter's format (O-06). |
| `bench.task_unsupported` | `unsupported` | no | A task needs something this build does not do (O-06). |
| `bench.isolation_unavailable` | `policy_denied` | no | Required benchmark isolation cannot be enforced, or the state overlaps Ogun's or Zeus's (O-06). |
| `bench.environment_failed` | `unavailable` | yes | The container runtime or a local process failed (O-06). |
| `bench.evaluator_failed` | `internal` | no | An evaluator produced no readable verdict (O-06). |
| `bench.spec_invalid` | `invalid_request` | no | A matrix or agent specification is invalid (O-06). |
| `zeus.not_linked` | `policy_denied` | no | No usable Zeus service credential, or `zeusd` refused it (O-08). |
| `zeus.unavailable` | `unavailable` | yes | `zeusd` is not running, the connection broke or a request timed out (O-08). |
| `zeus.server_unverified` | `unauthenticated` | no | The Zeus endpoint failed `server_proof` or the peer checks (O-08). |
| `zeus.wrong_kind` | `policy_denied` | no | The Zeus credential is not a `service` credential (O-08). |
| `zeus.call_failed` | `unavailable` | yes | `zeusd` answered with a retryable error; `details.zeus_code` (O-08). |
| `zeus.refused` | `policy_denied` | no | `zeusd` answered with a non-retryable error; `details.zeus_code` (O-08). |
| `zeus.protocol_error` | `internal` | no | A `zeusd` answer did not have the shape Zeus 05 specifies (O-08). |
| `zeus.request_invalid` | `invalid_request` | no | A session request breaks O-08 section 6. |
| `zeus.path_outside_root` | `policy_denied` | no | No Zeus root is registered, or a job's file is outside it (O-08). |
| `zeus.job_not_completed` | `conflict` | no | Import of a Zeus job that has not completed (O-08). |
| `zeus.import_failed` | `conflict` | no | Ogun's artifact store refused an import from Zeus (O-08). |

A code keeps its meaning and class forever; a replaced code is deprecated, never reused. Diagnostics (O-02 section 10) use the same lexical form in the `ir` namespace but are not errors.
