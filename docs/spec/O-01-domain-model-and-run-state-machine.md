# O-01. Domain model and run state machine

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, Zeus 01 (domain model), Zeus 13 (artifacts) |
| Implemented by | `ogun-run` (section 5) |

## 1. Purpose

This spec names Ogun's entities and defines how a run and its node instances change state. The state machine is table-driven: the table in section 5.2 is the only way a node instance changes state, and the code's table is tested against it.

## 2. Entities

| Entity | Id | Meaning |
|---|---|---|
| Workflow | `wf_` | A name and its versions. Each version is an IR document (O-02), identified by its workflow hash. |
| Run | `orun_` | One durable execution of one workflow version: node instances, artifact edges, checkpoints, events and costs. |
| Node instance | `ni_` | One node of a run for one partition. Unpartitioned workflows have one implicit partition. |
| Artifact | `art_` | An immutable materialization in Ogun's `zeus-artifact` store (Zeus 13). Identity across stores is its BLAKE3 digest. |
| Asset | `ast_` | A logical object with versions; each version points at artifacts. |
| Approval | `oap_` | A human decision bound to a request hash (O-07, planned). |
| Environment | `env_` | The isolated place a run executes (O-06, planned). |
| Evaluator | `evl_` | Something that scores a run (O-06, planned). |
| Agent run | `agr_` | A recorded agent run (O-05, planned). |

A run is bound to exactly one workflow hash for its whole life. Changing the workflow produces a new version and, for a new run, a new hash.

## 3. Partitions

A run over a partitioned workflow has one node instance per node and partition. Partitions succeed, fail and retry independently. A backfill is a run, or a series of runs, over a range of partitions; a partition whose outputs are already committed for the same workflow hash is `skipped` with cause `up_to_date`.

## 4. Durability

Every transition is committed to the run store before its effects are visible: events, child dispatch or the next transition. After a crash, `ogund` finds each node instance that was `running` and applies `recovered`, which returns it to `ready`. Operators resume from their last checkpoint; acquisitions resume from the Zeus engine's journal. Outputs are committed to the artifact store before `outputs_committed`, so a recovered node never loses or duplicates a committed artifact.

## 5. State machine

### 5.1 Node states

| State | Terminal | Meaning |
|---|---|---|
| `pending` | no | Declared; inputs not yet available. |
| `ready` | no | Inputs available; waiting for a worker slot or a budget reservation. |
| `running` | no | An operator is executing. |
| `waiting` | no | Blocked on an external condition or trigger. |
| `waiting_for_user` | no | Needs a human approval on an Ogun trust surface. |
| `waiting_for_session` | no | Needs a browser session through the Zeus session bridge. |
| `retry_scheduled` | no | Failed transiently; backing off before the next attempt. |
| `succeeded` | yes | Outputs materialized and verified. |
| `failed` | yes | Failed permanently, with a typed error. |
| `cancelled` | yes | Stopped by a person or a policy. |
| `skipped` | yes | Not needed: a route not taken, a denied approval or an up-to-date partition. |

### 5.2 Transitions

Each row is `from`, `cause`, `to`, and the only actor that may cause it. Actors: `runtime` (the scheduler inside `ogund`), `operator` (an operator process, through the ABI), `human` (a person on an Ogun trust surface: the CLI on a tty or Studio), `zeus` (zeusd, through the session bridge).

<!-- transitions:begin -->
| From | Cause | To | Actor |
|---|---|---|---|
| `pending` | `inputs_available` | `ready` | `runtime` |
| `pending` | `not_needed` | `skipped` | `runtime` |
| `pending` | `cancelled` | `cancelled` | `runtime` |
| `ready` | `dispatched` | `running` | `runtime` |
| `ready` | `up_to_date` | `skipped` | `runtime` |
| `ready` | `condition_unmet` | `waiting` | `runtime` |
| `ready` | `approval_required` | `waiting_for_user` | `runtime` |
| `ready` | `cancelled` | `cancelled` | `runtime` |
| `running` | `outputs_committed` | `succeeded` | `runtime` |
| `running` | `retryable_error` | `retry_scheduled` | `operator` |
| `running` | `permanent_error` | `failed` | `operator` |
| `running` | `approval_required` | `waiting_for_user` | `runtime` |
| `running` | `session_required` | `waiting_for_session` | `runtime` |
| `running` | `recovered` | `ready` | `runtime` |
| `running` | `cancelled` | `cancelled` | `runtime` |
| `waiting` | `condition_met` | `ready` | `runtime` |
| `waiting` | `deadline_passed` | `failed` | `runtime` |
| `waiting` | `cancelled` | `cancelled` | `runtime` |
| `waiting_for_user` | `approval_granted` | `ready` | `human` |
| `waiting_for_user` | `approval_denied` | `skipped` | `human` |
| `waiting_for_user` | `approval_expired` | `failed` | `runtime` |
| `waiting_for_user` | `cancelled` | `cancelled` | `runtime` |
| `waiting_for_session` | `session_available` | `running` | `zeus` |
| `waiting_for_session` | `session_unavailable` | `failed` | `zeus` |
| `waiting_for_session` | `cancelled` | `cancelled` | `runtime` |
| `retry_scheduled` | `backoff_elapsed` | `ready` | `runtime` |
| `retry_scheduled` | `cancelled` | `cancelled` | `runtime` |
<!-- transitions:end -->

Rules:

1. A transition not in the table is rejected with `run.invalid_transition`, including one attempted by the wrong actor.
2. Only `human` decides an approval. No agent-reachable path (MCP, A2A, the API on behalf of an agent, the scheduler) maps to `human` (Zeus ADR 0007).
3. Terminal states have no exits. Every non-terminal state can be cancelled.
4. `retryable_error` is used only while retries remain under `policy.retries`; the last failure is `permanent_error`.
5. Cancellation is a cause the runtime applies on behalf of a person or a policy; the decision to cancel is recorded with its origin in the run's events.

### 5.3 Run state

A run's state is derived from its node instances, never stored independently:

| Run state | Condition, checked in this order |
|---|---|
| `failed` | Every node instance is terminal and at least one `failed`. |
| `cancelled` | Every node instance is terminal, none `failed`, at least one `cancelled`. |
| `succeeded` | Every node instance is `succeeded` or `skipped`. |
| `running` | At least one is `ready`, `running` or `retry_scheduled`. |
| `waiting` | At least one is `waiting`, `waiting_for_user` or `waiting_for_session`. |
| `running` | At least one has left `pending`. |
| `pending` | Otherwise. |

### 5.4 Engine job states

`acquire` and `zeus.session_acquire` nodes mirror the state of their Zeus engine job (Zeus 01):

| Zeus job state | Node state |
|---|---|
| `queued` | `ready` |
| `probing`, `downloading`, `paused`, `verifying`, `finalizing` | `running` |
| `awaiting_approval` | `waiting_for_user` |
| `awaiting_session` | `waiting_for_session` |
| `retry_scheduled` | `retry_scheduled` |
| `completed` | `succeeded` |
| `failed` | `failed` |
| `canceled` | `cancelled` |

A paused job keeps its node `running`: pausing is a control action on the transfer, not a change in what the node does.
