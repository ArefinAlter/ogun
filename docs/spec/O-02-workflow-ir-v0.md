# O-02. Workflow IR v0

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-01, O-03 (catalogue), Zeus 00 section 7, Zeus 08 section 12 |
| Decided by | Proposed ADR 0003 |
| Implemented by | `ogun-ir`; schema [`schema/workflow-ir.v0.schema.json`](schema/workflow-ir.v0.schema.json) |
| Examples | [`examples/`](../../examples) |

## 1. Purpose

A workflow is a versioned JSON document: a DAG of operator nodes with their inputs, configuration, routes and policy. It contains no code and no floating-point numbers, so it can be hashed, diffed and approved before it runs. YAML, the Python SDK and an agent's draft all compile to this form; nothing reaches the runtime except through the parser and validator of this spec.

## 2. Parsing

A document is processed in this order. The first step that fails produces exactly one error diagnostic and stops processing:

1. **Size.** At most 1 MiB of UTF-8. Otherwise `ir.too_large`.
2. **Syntax.** One JSON value. Otherwise `ir.json_syntax`, reporting line and column.
3. **Hashing rules** of Zeus 00 section 7: no floating-point numbers, integers within ±(2^53 − 1), no duplicate object members. Otherwise `ir.not_hashable`.
4. **Version.** `ir_version` is an integer this build reads (0). Otherwise `ir.version_unsupported`, or `ir.schema` if absent.
5. **Shape.** The document matches sections 3 to 7: required members present, no unknown members anywhere except inside `config`. Otherwise `ir.schema`.

Then the validator (section 10) reports every finding it can, not just the first.

## 3. Document

| Member | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `ir_version` | integer | yes | | `0`. |
| `workflow` | string | yes | | Name, `^[a-z][a-z0-9-]{0,62}$`. Stable across versions. |
| `version` | integer | yes | | From 1; every change increases it. |
| `description` | string | no | | At most 4096 bytes. Not interpreted. |
| `triggers` | array | no | `[]` | Section 4.1. Empty means manual only. |
| `partitions` | object | no | | Section 4.2. Absent means one implicit partition. |
| `budgets` | object | no | `{}` | Section 4.3. Limits per run (per partition when partitioned). |
| `nodes` | array | yes | | 1 to 1000 nodes, section 5. |

## 4. Triggers, partitions and budgets

The full scheduling model is O-04. IR v0 carries:

### 4.1 Triggers

Each trigger is an object with `kind`: `manual`; `cron` with a five-field `cron` expression (minute, hour, day of month, month, day of week; `*`, lists, ranges and `/step`; evaluated in UTC); or `interval` with `interval_ms` of at least 60 000. At most 16.

### 4.2 Partitions

- `{"kind": "time", "granularity": "day" | "month", "start": ..., "end": ...}`: one partition per UTC day (`YYYY-MM-DD`) or month (`YYYY-MM`). `end` is inclusive and optional; it is not before `start`.
- `{"kind": "static", "keys": [...]}`: 1 to 10 000 distinct keys, `^[A-Za-z0-9._-]{1,64}$`.

### 4.3 Budgets

Optional members `bytes`, `files`, `wall_ms` and `llm_usd_micro`, each a positive integer. An absent member is unlimited at that level; `0` is an error (`ir.budget_zero`). A node's `policy.budgets` member must not exceed the workflow's member of the same name.

## 5. Nodes

| Member | Type | Default | Meaning |
|---|---|---|---|
| `id` | string | | `^[a-z][a-z0-9_]{0,62}$`, unique in the workflow. |
| `op` | string | | Operator reference `name@major` (section 6). |
| `inputs` | array | `[]` | 0 to 32 references `node_id.output` to another node's declared output. |
| `config` | object | `{}` | Operator configuration, section 6.2. |
| `route` | array | `[]` | 0 to 16 routing edges, section 5.1. |
| `policy` | object | see 5.2 | Retries, timeout, concurrency, approval, budgets. |
| `code` | object | | `{"blake3": <64 hex>, "path": <optional string>}`: the code a code operator runs. |

Edges come from `inputs` (producer to consumer) and `route` (router to target). The graph MUST be acyclic (`ir.cycle`).

A node that has no inputs, is no route target and whose operator cannot start a DAG gets the warning `ir.node_has_no_inputs`. Operators that can start a DAG: `acquire`, `zeus.session_acquire`, `crawl`, `duckdb`, `git`, `hf`, `rclone`, `yt_dlp`, and every code operator.

### 5.1 Routes and predicates

A route `{"when": <predicate>, "to": <node id>}` sends the items that match `when` to `to` instead of downstream. The target MUST exist, MUST NOT be the router, MUST declare no `inputs` of its own and MUST be the target of only one router. It receives the routed items as its input.

Predicates are a closed set. Each is an object with exactly one member:

| Predicate | Form | Holds when |
|---|---|---|
| `all` | array of 1 to 16 predicates | all hold |
| `any` | array of 1 to 16 predicates | at least one holds |
| `not` | predicate | it does not hold |
| `eq` | `{"field", "value"}` | the metadata field equals the scalar (string, integer, boolean, null) |
| `in` | `{"field", "values"}` | the field equals one of 1 to 64 scalars |
| `exists` | `{"field"}` | the field is present and not null |
| `media_type` | `"type/subtype"` or `"type/*"` | the artifact's media type matches |
| `size_bytes` | `{"gte", "lt"}`, at least one | the size is in `[gte, lt)`; `gte < lt` |

Fields are dotted `snake_case` names of artifact metadata, at most 128 bytes. Predicates nest at most 8 levels. A missing field never satisfies `eq` or `in`.

### 5.2 Policy

| Member | Default | Range |
|---|---|---|
| `retries` | 3 | 0 to 20 |
| `timeout_ms` | 3 600 000 | at least 1 000 |
| `concurrency` | 4 | 1 to 64 |
| `approval` | `none` | `none`, `always` (before every run of the node), `over_budget` (when a reservation would exceed a budget) |
| `budgets` | `{}` | as section 4.3 |

## 6. Operators and configuration

### 6.1 References

`op` is `name@major`: dotted segments of `[a-z][a-z0-9_]*`, at most 64 characters, then `@` and a major version without leading zeros. The name MUST be in the operator catalogue (O-03 section 7) with that major version (`ir.op_unknown`, `ir.op_version_unknown`). Each input's `output` MUST be one the producing operator declares (`ir.input_unknown_output`).

### 6.2 Configuration rules

`config` belongs to the operator, which validates it. The IR adds these rules for every operator:

1. **No inline code.** Members named `code`, `command`, `query`, `script`, `source_code` or `sql` are rejected (`ir.inline_code`). Code is referenced by `code.blake3`.
2. **No inline secrets.** A member named like a secret (`access_key`, `api_key`, `apikey`, `authorization`, `client_secret`, `cookie`, `cookies`, `password`, `private_key`, `secret`, `secret_key`, `session_token`, `token`; case-insensitive) at any depth MUST be a secret reference `{"secret_ref": "<name>"}`, resolved from Ogun's credential store at run time (`ir.secret_inline`).
3. **No credentials in URLs.** A string of the form `scheme://user@host...` is rejected (`ir.url_credentials`).
4. **Acquire never holds a session** (Zeus 08 section 12 item 3). `acquire` configuration has no `session` member and no `infinite_scroll` anywhere (`ir.session_in_acquire`). Sources behind a browser login use `zeus.session_acquire`, which runs through zeusd.

The configuration of `acquire` uses the portable acquisition core of Zeus 08 (`allowed_domains`, `sources`, `pagination`, `extract`, `filters`, `fetch`, `budgets`, `stop`) with the same semantics and limits. Until that code moves to `zeus-core`, Ogun validates only the rules above for it.

## 7. Code, models and approvals

1. A node whose operator runs user code (`python`, `polars`, `sql`, `dbt`, `shell`) MUST have `code` (`ir.code_required`). A node whose operator runs none MUST NOT (`ir.code_unexpected`). `code.blake3` is 64 lowercase hex characters (`ir.code_hash_invalid`).
2. `shell` MUST have `policy.approval: always` (`ir.shell_needs_approval`).
3. A model operator (`llm.*`, `embedding.generate`) needs an `llm_usd_micro` budget on the node or the workflow (`ir.llm_budget_required`). A workflow with no model operator runs with no model access.

## 8. Workflow hash

### 8.1 Normal form

The normal form is the document with every member that has a default present with its value (`triggers`, `budgets`, and per node `inputs`, `config`, `route`, and every `policy` member), and optional members without a default absent. Two documents that differ only in whitespace, member order or spelled-out defaults have the same normal form.

### 8.2 Hash

The workflow hash is BLAKE3 (32 bytes, no key) of the JCS serialisation (RFC 8785) of the normal form, as 64 lowercase hex characters. Approvals and runs bind to it. Every change that can alter behaviour changes it; changes to `description` and `version` do too, deliberately, so a hash names exactly one document.

## 9. Plan diff

### 9.1 Content

Comparing an approved version with a proposed one yields: nodes added, removed and changed (per node, which of `op`, `inputs`, `config`, `route`, `policy`, `code` differ), budget members that differ (workflow and node level), whether triggers or partitions differ, and code hashes added and removed.

### 9.2 Approval

The proposed version needs a human approval bound to its hash before it runs if any of these hold:

| Reason | When |
|---|---|
| `code_changed` | A node with `code` is new, or its `code` or `op` changed. |
| `budget_increased` | A budget member grew or became unlimited. |
| `model_use_added` | The old version used no model operator and the new one does. |
| `always_approve_added` | A node gained `approval: always`. |

Otherwise it runs under the approvals of the old version. Removing nodes, lowering budgets and changing triggers never need an approval by themselves.

## 10. Diagnostics

A diagnostic is `{"severity", "code", "path", "message"}` as in Zeus 08 section 6.3. `path` is a JSON Pointer. `message` is English for people and never quotes a value from the document.

| Code | Severity | Rule |
|---|---|---|
| `ir.too_large`, `ir.json_syntax`, `ir.not_hashable`, `ir.version_unsupported`, `ir.schema` | error | Section 2 |
| `ir.name_invalid`, `ir.version_invalid`, `ir.description_too_long`, `ir.nodes_empty`, `ir.too_many_nodes` | error | Section 3 |
| `ir.trigger_invalid`, `ir.partitions_invalid`, `ir.budget_zero`, `ir.budget_exceeds_workflow` | error | Section 4 |
| `ir.node_id_invalid`, `ir.node_id_duplicate`, `ir.input_invalid`, `ir.input_self`, `ir.input_unknown_node`, `ir.input_unknown_output`, `ir.too_many_inputs`, `ir.too_many_routes`, `ir.cycle`, `ir.policy_out_of_range` | error | Section 5 |
| `ir.route_target_unknown`, `ir.route_self`, `ir.route_target_has_inputs`, `ir.route_target_shared`, `ir.predicate_invalid` | error | Section 5.1 |
| `ir.node_has_no_inputs` | warning | Section 5 |
| `ir.op_invalid`, `ir.op_unknown`, `ir.op_version_unknown`, `ir.inline_code`, `ir.secret_inline`, `ir.url_credentials`, `ir.session_in_acquire` | error | Section 6 |
| `ir.code_required`, `ir.code_unexpected`, `ir.code_hash_invalid`, `ir.shell_needs_approval`, `ir.llm_budget_required` | error | Section 7 |

## 11. Limits

| Item | Limit |
|---|---|
| Document | 1 MiB |
| Nodes | 1000 |
| Inputs per node | 32 |
| Routes per node | 16 |
| Predicate arms / `in` values / depth | 16 / 64 / 8 |
| Triggers | 16 |
| Static partition keys | 10 000 |
| Description | 4096 bytes |

## 12. Open issues

1. YAML authoring: the mapping is one to one, but YAML 1.2 number typing (floats) needs a rule before the loader exists.
2. Templates in `config` (Zeus 08 section 5.1) are validated only once the plan core moves to `zeus-core`.
3. Installed operator manifests (O-03) replace the built-in catalogue's entries; the precedence rule is open.
