# O-05. Lineage and run records

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-01, O-03, Zeus 04 (provenance), Zeus 13 (artifacts) |
| Decided by | ADR 0004 decision 4 (lineage interoperates), ADR 0005 decisions 5 and 6 |
| Implemented by | `ogun-lineage` (`openlineage`, `export`, `records`); the facet schema [`schema/ogun-lineage-facets.v0.schema.json`](schema/ogun-lineage-facets.v0.schema.json) |

## 1. Purpose

A run leaves three kinds of record: its state history in the run store (O-01 section 4), the artifacts it committed in Ogun's `zeus-artifact` store (Zeus 13), and, for agent runs, a trajectory. This spec says how those become OpenLineage events other tools can read, and fixes the shapes of the agent-run record, the trajectory and the cost ledger that the benchmark profile (O-06) writes.

Lineage describes what happened. Nothing in it decides anything, and nothing in it is read back as an instruction.

## 2. Inputs

The builder takes rows, not connections, so it runs in `ogund`, in the CLI or in Studio alike:

| Input | Source | Notes |
|---|---|---|
| Run record | `ogun_store::RunRecord` | Workflow name, version, hash, partitions. |
| Node instances | `ogun_store::NodeInstance` | Current state of each. |
| Events | `ogun_store::Event` | The run's transitions, in `seq` order. Events of other runs are ignored. |
| Outputs | `OutputArtifact` (this crate) | One row per committed artifact: node instance, output port, `art_` id, BLAKE3, SHA-256, size, media type, commit time. The daemon copies them from its artifact index. |
| Operators | `OperatorInfo` per node | `name@major`, the exact version that ran, the manifest BLAKE3 and, for `code` operators, the code BLAKE3 (O-03 section 6). |
| Node inputs | the workflow IR | `node.output` references, for input datasets. |
| Windows | per partition key | `start_ms`, `end_ms`, for windowed partitions (O-09). |
| Errors | per failed node instance | The final O-00 envelope; its message is already free of secrets. |

An output or event that names a node instance of another run is rejected (`internal.bug`): the rows disagree, and guessing would produce false lineage.

## 3. Identity

1. **Namespace.** Jobs and datasets live in one namespace, default `ogun://local`. A self-hosted server uses `ogun://<workspace>` (O-13); benchmark matrices use `ogun://bench` unless configured.
2. **Jobs.** An Ogun run is a run of the job `<workflow>`. A node instance is a run of the job `<workflow>.<node>`. An agent run is a run of the job `bench.<benchmark>.<task_id>`.
3. **Run ids.** OpenLineage run ids are UUIDs. Ogun derives one from each ULID (`orun_`, `ni_`, `agr_`): the 128 bits of the ULID with the version nibble set to 7 and the variant bits to `10`. Both start with a 48-bit millisecond timestamp, so the order is kept. The mapping loses six bits and is not reversed; the Ogun id is carried in a facet.
4. **Datasets.** A dataset is one output port of one node: `<workflow>.<node>.<output>`. Its version for a partition is the `datasetVersion` facet: BLAKE3 over the JCS form of the list of `{blake3, size_bytes}` the port committed for that partition, sorted. Two runs that commit the same bytes produce the same version.

## 4. Event mapping

### 4.1 Node instances

| Store transition | OpenLineage event | Contents |
|---|---|---|
| first `ready -> running` (`dispatched`) | `START` | Input datasets with versions |
| later `dispatched`; `waiting_for_session -> running`; `running -> retry_scheduled`, `waiting_for_user`, `waiting_for_session`, `ready` (`recovered`) | `RUNNING` | State and cause in `ogun_node_run` |
| `running -> succeeded` (`outputs_committed`) | `COMPLETE` | Input and output datasets with versions and artifacts |
| any `-> failed` | `FAIL` | `errorMessage` and `ogun_error` when the error is known |
| `-> cancelled` after a start | `ABORT` | |
| anything else (`pending -> ready`, skips, cancels before a start) | none | |

### 4.2 Runs

The Ogun run emits `START` at its `run_created` event. When every node instance is terminal it emits `COMPLETE`, `FAIL` or `ABORT` for the derived run state (O-01 section 5.3), at the time of the last event. Builders run on a live run emit only what has happened so far; calling again later yields a superset, in the same order.

### 4.3 Facets

Standard OpenLineage facets used: `parent` (every node run points at its Ogun run), `processing_engine` (`ogun` and its version), `jobType` (`processingType: BATCH`, `integration: OGUN`, `jobType: WORKFLOW | NODE | AGENT_RUN`), `errorMessage`, and the dataset facet `version`.

Ogun's own facets are prefixed `ogun_` and use `snake_case` members, with `_producer` and `_schemaURL` as every facet must. They are described in [`schema/ogun-lineage-facets.v0.schema.json`](schema/ogun-lineage-facets.v0.schema.json).

| Facet | On | Members |
|---|---|---|
| `ogun_run` | Ogun run | `run_id` (`orun_`), `workflow`, `workflow_version`, `workflow_hash`, `partitions` (count), `state` |
| `ogun_node_run` | node run | `node_instance` (`ni_`), `run_id`, `workflow_hash`, `node`, `partition`, `attempt`, `state`, `cause` |
| `ogun_operator` | node run | `op`, `version`, `manifest_blake3`, `code_blake3` |
| `ogun_partition` | node run | `key`, `window_start_ms`, `window_end_ms` |
| `ogun_error` | failed node run | `code`, `class`, `retryable` |
| `ogun_artifacts` | dataset | `partition`, `count`, `total_bytes`, `truncated`, `artifacts[]` of `{blake3, sha256, size_bytes, media_type, artifact_id, committed_at}`, at most 256 listed |
| `ogun_agent_run` | agent run | the agent-run record of section 6 |

OpenLineage's own members are camelCase (`eventType`, `runId`, `schemaURL`); that is the standard's convention and Ogun keeps it in exported documents only.

### 4.4 What never appears

No facet carries a value copied from a node's configuration, a URL that has not been through `redact_url`, a secret, a local path, or text from a remote server. Every number is an integer, so an event is hashable as Zeus 00 section 7 requires.

## 5. Export

1. **JSON Lines.** Events are written one compact document per line, each followed by `\n`, in the order of section 4. Appending to an existing file is allowed; readers skip empty lines.
2. **HTTP.** A person may configure an OpenLineage endpoint (for example Marquez). Events are posted one per request, in order, as `application/json`, to the configured URL; a URL with an empty path posts to `/api/v1/lineage`. An optional API key is sent as `Authorization: Bearer <key>`. A 2xx status is success; anything else stops the export at that event (`lineage.endpoint_failed`), and the caller resumes from it.
3. **Redaction.** The endpoint URL reaches logs and errors only as `redact_url` output (mode `log`). The URL and the key are held in types without `Serialize`; the key is zeroized. A URL with user information is refused.
4. **Transport.** This build speaks HTTP/1.1 itself and reaches `http://` endpoints: loopback, a LAN service, or a local TLS-terminating forwarder. An `https://` endpoint is refused with `lineage.endpoint_unsupported` rather than sent in clear text (open issue 1).
5. Nothing is sent unless a person configured an endpoint. There is no default endpoint and no telemetry.

## 6. Agent-run records

An agent run (`agr_`) is one execution of one agent on one task in one environment with one seed (O-06). Its record:

| Member | Meaning |
|---|---|
| `id` | `agr_` |
| `agent` | `name`, `version`, `model`, `config_blake3` (BLAKE3 of the JCS form of the agent configuration: command, credential names, limits; never credential values), `prompt_blake3`, `tools_blake3` |
| `task` | `benchmark`, `task_id`, `task_version`, `task_blake3` (O-06 section 5) |
| `environment`, `environment_blake3` | `env_` and the environment digest (O-06 section 4.3) |
| `evaluator`, `evaluator_version` | `evl_` and `name@version` |
| `seed`, `run_id` | The seed; the Ogun run when the agent run is part of one |
| `started_at`, `finished_at`, `outcome` | `completed`, `failed`, `timed_out`, `cancelled` or `error` (the harness failed, not the agent) |
| `exit_code`, `agent_ms`, `wall_ms` | The agent process and the whole run |
| `totals` | `input_tokens`, `output_tokens`, `cost_usd_micro`, `bytes`, `model_requests` |
| `tool_calls`, `files_created`, `files_modified`, `files_deleted` | Counts |
| `trajectory` | `path` (relative to the results directory), `events`, `head_blake3` |
| `score` | `passed`, `score_ppm` (0 to 1,000,000), `result_blake3` (the evaluator's full result) |
| `cached` | The result came from the result cache (O-06 section 8) |
| `error_code` | The harness's O-00 code when `outcome` is `error` |

## 7. Trajectories

A trajectory is an append-only JSON Lines file, written once (the file must not exist). Every line is a `TrajectoryEvent`:

1. `seq` from 1 without gaps; `at` (when the harness recorded it); `elapsed_ms` since the run started; `agent_run`; and `prev_blake3`, the BLAKE3 of the JCS form of the previous line (absent on the first). A reader checks both and stops at the first break (`lineage.trajectory_broken`).
2. `type` is one of: `task.started`, `workspace.snapshot`, `agent.started`, `message`, `model.request`, `model.response`, `tool.call`, `tool.shell`, `tool.result`, `file.read`, `file.created`, `file.patch`, `file.deleted`, `agent.output`, `usage`, `agent.exited`, `db.state`, `evaluator.started`, `evaluator.finished`, `task.finished`.
3. Text from an agent, a model or a tool is stored only in members named `untrusted_*`, at most 64 KiB each (longer text is cut at a character boundary and flagged `truncated`). Before it is written, every secret value the harness knows (the model credentials it passed in) is replaced by `[redacted]`, and the leak marker `ZEUS-CANARY-` is defused.
4. An agent may report its own events by writing lines to its trajectory log (O-06 section 7). Only `message`, `model.request`, `model.response`, `tool.call`, `tool.shell`, `tool.result`, `file.read` and `usage` are accepted from an agent; the harness-only types are refused and counted, so an agent cannot forge its exit, its evaluation or its file changes.
5. A summary sums tokens and cost from `model.*` and `usage`, counts tool calls, failed tool calls (`tool.result` with a non-zero exit code), shell commands repeated verbatim and files read again: the proxies for tool efficiency in VISION section 12.

`ogun-bench replay <trajectory>` verifies a trajectory and prints it.

## 8. Cost ledger

A cost-ledger entry is `{at, kind, run_id?, node_instance?, agent_run?, provider?, model?, input_tokens, output_tokens, bytes, wall_ms, cost_usd_micro, estimated}`, where `kind` is `model`, `compute`, `network` or `storage`. Money is integer micro-dollars; `estimated` is true when no provider invoice line backs the amount. Operators report model use as `OpEvent::Usage` (O-03); the daemon turns each into a `model` entry with the run and node instance set.

## 9. Error codes

Registered in O-00 section 5.2:

| Code | Class | Retryable | Raised when |
|---|---|---|---|
| `lineage.io_failed` | `internal` | no | An export file cannot be read or written. |
| `lineage.endpoint_unsupported` | `invalid_request` | no | The configured endpoint is not an `http://` URL without user information. |
| `lineage.endpoint_failed` | `unavailable` | yes | The endpoint could not be reached, timed out, or answered with a non-2xx status. |
| `lineage.trajectory_broken` | `conflict` | no | A trajectory has a sequence gap or a broken BLAKE3 chain. |

## 10. Open issues

1. HTTPS to remote OpenLineage endpoints: needs a TLS client in the workspace (the `zeus-core` HTTP stack is the candidate) rather than a second one.
2. Artifact-level lineage across products: an artifact imported from Zeus (O-08) carries Zeus's provenance in its record. A facet that exposes the import hop (`imports[]` of Zeus 13 section 10) would let a lineage graph cross from the Zeus download to the Ogun dataset.
3. Agent-reported trajectory events are stamped when the harness ingests them, after the agent exits. Live ingestion (tailing the log) would keep the agent's own timing.
4. The OpenLineage facet schema versions cited in the code (`ParentRunFacet` 1-1-0, `JobTypeJobFacet` 2-0-3, `ErrorMessageRunFacet` 1-0-1, `ProcessingEngineRunFacet` 1-1-1, `DatasetVersionDatasetFacet` 1-0-1, `RunEvent` 2-0-2) are unverified against the current OpenLineage release.
5. W3C PROV and Croissant exports of artifact provenance go through Zeus 04's manifest exports (ADR 0004 decision 4) and are not repeated here.
