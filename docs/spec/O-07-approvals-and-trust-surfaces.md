# O-07. Approvals and trust surfaces

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-01, O-02 section 9, O-15 |
| Decided by | ADR 0002 decision 6, ADR 0003 decision 3, ADR 0010 decision 4, ADR 0015 (Windows peers, the Studio surface); Zeus ADR 0007 applied unchanged |
| Implemented by | `ogun-daemon` (`approvals`, `webauthn`), `ogun-api` (policy table, evidence types), `ogun approve` and `ogun deny` in `ogun-cli` |

## 1. Purpose

An approval is a request for a person's decision before Ogun does something that a workflow author, an agent or a schedule must not be able to do alone: run code nobody approved, spend more, call a model where none was called, or run a node whose policy asks every time. This spec says what the person sees, who can decide, how a decision is bound to the request, and what happens when nobody decides.

## 2. Kinds

| Kind | Raised when | Scope |
|---|---|---|
| `code_change` | `workflows.put` stores a version that references a code hash not approved before for this workflow (O-02 section 9.2) | The version: `runs.start` refuses it with `workflow.not_approved` until approved |
| `budget` | A version's budget grew or became unlimited compared with the previous version | The version |
| `policy` | A version adds a model operator where there was none; or, at run time, a node whose `policy.approval` is `always` becomes ready | The version, or one node instance |
| `egress` | Reserved for network origins not approved before (with the sandbox's allowlists) | The version |
| `publish` | Reserved for nodes that publish outside the workspace | One node instance |

An approved code hash stays approved for that workflow: rolling back to it, or keeping it in a later version, needs no new approval. Approvals never cover session release; that is always a Zeus approval on a Zeus trust surface (ADR 0002).

## 3. What the person sees

1. Facts computed by the daemon only: the workflow name and version, the workflow hash, node ids and operator names, code hashes, budget members, the partition, the reason. Node ids and operator names are constrained by the IR's patterns; no free text from the workflow (its description, configuration values) and no text from an agent appears among the facts.
2. The requester (credential label and kind), the creation and expiry times.
3. The `request_hash`: BLAKE3 over the JCS form of the approval id, kind, workflow, workflow hash, run, node, facts and expiry.

## 4. Who decides

| Channel | Evidence | Checked by the daemon |
|---|---|---|
| Terminal | `{"channel": "terminal"}` | The connection's credential is `human`; the peer runs a pinned `ogun` binary, and on Linux has a controlling terminal (O-15 section 4.4; on Windows the pinned image path is the whole check). The CLI shows the facts and asks the person to type the first eight characters of the request hash, read from `/dev/tty` (`CONIN$` on Windows), never from stdin. |
| Studio | `{"channel": "studio"}` | The credential is `human` and the peer runs the pinned `ogun-studio` binary. Studio shows the facts and needs an explicit click that names the decision. |
| WebAuthn | `{"channel": "webauthn", ...}` | The connection is a `bridge`; remote approvals are enabled; the assertion verifies against an approver key a person confirmed on the host terminal (O-15 section 7). |

Every other combination is `approval.channel_required`, and agents and services are refused with `api.permission_denied` before evidence is looked at. No MCP tool, A2A skill, API token, integration or scheduler path can reach a decision (constraint 3).

## 5. Binding and limits

1. A decision names the approval's `request_hash`; a mismatch is `approval.hash_mismatch`.
2. A decision is final: a second decision is `approval.already_decided`.
3. An approval expires after `approval_ttl_ms` (default 7 days). Expiry is a denial: a waiting node instance fails with cause `approval_expired`.
4. At most 50 approvals are pending; at most 10 decisions are accepted per minute.
5. A version-scoped approval gates every run of that version; a node-scoped approval moves exactly one node instance (`approval_granted` to `ready`, `approval_denied` to `skipped`). A granted node instance is not asked again when it retries.
6. Approving records the code hashes it covered for the workflow, in the same transaction as the decision.

## 6. Honest limit

As Zeus ADR 0007 point 5: a program that runs as the same OS user with shell access can read the same credential files and drive a pseudo-terminal. The kind separation, the peer checks, the typed confirmation, the request hash, the rate limits and expiry narrow this; they do not remove it. On Windows the daemon inspects the pipe's client (process id and executable image, O-15 section 4.4), so terminal and Studio approvals work there; on macOS it cannot yet, so no connection is interactive there and approvals are unavailable until the platform module exists; Studio-web with WebAuthn is the alternative. On Windows the check for the terminal surface is the pinned executable's path alone, because Windows offers no equivalent of a controlling terminal to test.

## 7. Conformance

`cargo test -p ogun-api` checks that the policy table never lets an agent or a service call `approvals.decide`, and `cargo test -p ogun-daemon` checks the refusal over a real connection.
