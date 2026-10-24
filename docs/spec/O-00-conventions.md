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

Ogun identifiers are prefixed ULIDs with exactly the syntax of Zeus 00 section 5.1. Only `ogund` mints them. The prefixes below are Ogun's; none equals a Zeus prefix. Until Zeus 00 section 5.1 lists them (a Zeus pull request), this table is the reservation.

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

Artifacts use `art_` from the `zeus-artifact` store (Zeus 13): Ogun runs its own store instance through that crate, and the id is local to the store, while the BLAKE3 digest is the identity across stores.

## 4. Local API transport

`ogund` has one listener, a local IPC endpoint (Unix domain socket, or a named pipe on Windows), speaking JSON-RPC 2.0 as Zeus 05 does. HTTP surfaces (MCP over HTTP, A2A) are separate bridge processes, off by default (Zeus ADR 0010 pattern). The method set is specified with the runtime; it is not part of Draft 1.

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

A code keeps its meaning and class forever; a replaced code is deprecated, never reused. Diagnostics (O-02 section 10) use the same lexical form in the `ir` namespace but are not errors.
