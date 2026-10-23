# O-14. Extension and integration surfaces

| | |
|---|---|
| Status | Draft 2 (2026-10-23: the language server, daemon-backed MCP tools, the Python client and the data-platform integrations) |
| Depends on | O-02, O-04, O-15 |
| Decided by | ADRs 0011 and 0012 |
| Implemented by | `ogun-lsp` (library `ogun_lsp::serve_stdio`, binaries `ogun-lsp` and `ogun lsp`); `ogun mcp` and `--json` output in `ogun-cli`; `extensions/vscode`; `integrations/` (`mcp` client configurations, `python/ogun-client`, `airflow`, `dagster`, `prefect`, `kestra`) |

## 1. Purpose

This spec defines how editors, coding agents and other data platforms reach Ogun, and what none of them may do: decide an approval, raise a budget, manage credentials or secrets, or run user code outside an approved workflow.

## 2. Editors

### 2.1 The language server

`ogun lsp` and the standalone binary `ogun-lsp` run the same server (`ogun_lsp::serve_stdio`): the Language Server Protocol 3.17 over stdio. Every editor integration uses it or, as a fallback, `ogun validate --json` (section 4). Neither carries a validator of its own (ADR 0011 decision 1).

1. **Documents.** The server treats a document as workflow IR when its URI path ends in `.workflow.json`. It ignores every other document: no diagnostics and no features.
2. **Sync and positions.** Full-text sync. Positions count UTF-16 code units unless the client offers `utf-8` in `general.positionEncodings`, in which case the server picks UTF-8.
3. **Diagnostics** come only from `ogun_ir::check`, the validator the runtime uses, and are published on open and on every change. Each is placed at the member its JSON Pointer names, by a tolerant scanner that also reads unfinished documents:
   - a string or literal is covered whole;
   - an object or array that is a member value is covered by its member name, any other container by its first line;
   - the whole document (`""`) by its first character;
   - `ir.json_syntax` at the line and column the parser reports;
   - a pointer that does not resolve falls back to its longest existing prefix.

   `source` is `ogun` and `code` is the diagnostic code. Diagnostics MUST NOT quote configuration values (O-02 section 10).
4. **Features.**

   | Request | What the server answers |
   |---|---|
   | `textDocument/hover` | On `op`: the operator's kind, what that kind means (user code, model spend), outputs and major versions. On a node id or a reference to one: its operator, its outputs as `node.output`, the nodes that read it and its router. On an input reference: the output and its producer, or why it does not resolve. On a member name of a known object: what the member means. On the workflow name: the workflow hash and the review summary when the document is valid. Nothing inside a node's `config`. |
   | `textDocument/completion` | `op` values `name@major` from the catalogue; `inputs` references `node.output` from the document's other nodes and their catalogue outputs; node ids for route targets; stream source ids for `triggers[].source`, `partitions.source` and `streams` members; member names of the document, nodes, `policy`, `budgets`, routes, predicates (and their `eq`, `in`, `exists`, `size_bytes` bodies), `code`, triggers, `partitions` and stream buffers; the closed value sets of `policy.approval`, trigger and partition `kind`, `granularity` and `when_full`. Trigger characters `"`, `.`, `@`. |
   | `textDocument/definition` | From an input reference, a route target, a stream source reference or a `streams` member to the `id` of the node it names. |
   | `textDocument/documentSymbol` | The workflow, with one child per node (name: the id, detail: the operator). |
   | `textDocument/codeLens` | On a valid document, one lens on the first line, `workflow hash <short> · v<version> · <n> nodes`, with command `ogun.workflowHash` and the full hash as its argument. |
   | `workspace/executeCommand` | `ogun.workflowHash`: the server shows the full hash with `window/showMessage`. A client that knows the command (the VS Code extension) copies the hash instead. |
   | `ogun/workflowHash` | Custom request, params `{"textDocument": {"uri"}}`, result `{"workflow_hash": <hex or null>, "errors": n, "warnings": n}`, for status bars. |

5. **Read-only.** The server never runs a workflow, never connects to `ogund` and is never an approval surface. It uses the built-in operator catalogue.

Any LSP client can use it: command `ogun lsp` (or `ogun-lsp`), transport stdio, file pattern `*.workflow.json`, language JSON. Neovim and Helix need only that configuration; JetBrains IDEs use an LSP client plugin, and Zed an extension that registers the command.

### 2.2 VS Code

`extensions/vscode` does five things:

- bundles the IR JSON Schema for `*.workflow.json`, for structural checks;
- in **language-server mode**, starts the server of section 2.1 and uses its diagnostics, hover, completion, definition, symbols and code lens;
- in **CLI mode**, runs `ogun validate --json` on open and on save, and places each diagnostic at the member its JSON Pointer names;
- shows the workflow hash in the status bar (from `ogun/workflowHash` or `ogun hash --json`);
- copies the hash on **Ogun: Copy workflow hash** or from the code lens.

It chooses the mode at activation. `ogun.languageServer.mode` is `auto` (the default), `on` or `off`. In `auto` and `on` it starts `ogun.languageServer.path` when set, otherwise `ogun-lsp` from `PATH`, otherwise `ogun lsp` with `ogun.path`. If no server is found, the server fails to initialise within 10 s, or it exits later, the extension uses CLI mode. Its LSP client is plain JavaScript (Content-Length framed JSON-RPC over the child's stdio); the extension has no npm dependencies.

The bundled schema MUST equal `docs/spec/schema/workflow-ir.v0.schema.json`, and a test checks it.

## 3. MCP

### 3.1 Transport

`ogun mcp` speaks MCP over stdio as newline-delimited JSON-RPC 2.0. It supports protocol revisions `2025-11-25`, `2025-06-18` and `2025-03-26`. It answers `initialize` with the client's requested revision when it supports it, and with the newest otherwise. It announces `tools` with `listChanged: false`.

A tool failure (an invalid workflow, an unreachable daemon, a daemon error) is a tool result with `isError: true`, so the agent can read and fix it. Missing or malformed arguments are JSON-RPC error `-32602`; the message never quotes the argument's value.

### 3.2 Offline tools

Always listed, read-only (`readOnlyHint: true`), and usable with no daemon and no credential.

| Tool | Arguments | Result |
|---|---|---|
| `ogun_validate` | `workflow`: IR as JSON text | Diagnostics; for a valid workflow, the hash and a summary |
| `ogun_diff` | `old`, `new` | The plan diff, `needs_approval` and both hashes |
| `ogun_partitions` | `workflow`, optional `from` and `to` | Partition count and the first 100 keys |
| `ogun_operators` | none | The operator catalogue |

### 3.3 Daemon tools

1. **Credential.** The daemon tools connect to `ogund` with `ogun_api::Client` and the local credential named by `OGUN_MCP_CREDENTIAL` (default `mcp`), in the state directory of `OGUN_HOME` or the platform default, for the profile `OGUN_PROFILE`. A person creates it once on a terminal: `ogun credentials add mcp --kind agent`. `ogun mcp` MUST refuse a credential of any kind other than `agent`, so an agent never borrows a person's authority.
2. **Listing.** The daemon tools are listed when the handshake succeeds (filtered to the methods the daemon reports for the connection) or, when `ogund` is not reachable, when the credential file exists, so they work once the daemon starts. `OGUN_MCP_OFFLINE=1` lists the offline tools only.
3. **Tools.** Each maps to exactly one O-15 method that the policy table allows the `agent` kind, and a test checks that mapping.

   | Tool | Method | Arguments |
   |---|---|---|
   | `ogun_daemon_status` | `daemon.status` | none |
   | `ogun_workflows_list` | `workflows.list` | none |
   | `ogun_workflow_get` | `workflows.get` | `name`, `version?` |
   | `ogun_workflow_put` | `workflows.put` | `workflow` (JSON text), `idempotency_key` |
   | `ogun_run_start` | `runs.start` | `workflow`, `version?`, `workflow_hash?`, `partitions?` (selector, default `latest`), `idempotency_key` |
   | `ogun_run_status` | `runs.get` | `run_id` |
   | `ogun_runs_list` | `runs.list` | `workflow?`, `before?`, `limit?` |
   | `ogun_partitions_status` | `partitions.status` | `workflow`, `version?`, `from?`, `to?` |
   | `ogun_backfill_plan` | `backfills.plan` | `workflow`, `from`, `to` |
   | `ogun_approvals_list` | `approvals.list` | `state?` |
   | `ogun_streams_list` | `streams.list` | none |
   | `ogun_stream_status` | `streams.status` | `workflow`, `node` |

   `ogun_workflow_put` and `ogun_run_start` change state; they require an `idempotency_key` (1 to 128 printable ASCII characters) and are annotated `readOnlyHint: false`, `destructiveHint: false`, `idempotentHint: true`. Every other daemon tool is read-only. `ogun_workflow_put` validates the document locally first and returns the diagnostics as a tool error when it is invalid.
4. **Results.** The method's result, with these changes: the stored document of `ogun_workflow_get` is returned as `untrusted_workflow`; every string that holds a URL has its userinfo, query and fragment redacted (`redact_url` in log mode for http and https); `ogun_approvals_list`, and a stored version that needs approvals or a run that is `waiting`, carry a `note` that a person decides on the Ogun CLI or in Studio. Members named `untrusted_*` hold text written outside Ogun, and the server's `instructions` tell the agent to treat them as data.
5. **Never.** No tool decides an approval, raises a budget, manages credentials, secrets, approver keys or edges, syncs, pushes stream records, or stops the daemon. A run of a version that needs approval waits in `waiting_for_user` (O-01) until a person decides.

The connection runs on its own thread with its own async runtime, and reconnects once when it drops; the retried call is safe because mutating calls carry an idempotency key.

### 3.4 Client configuration

`integrations/mcp/` holds ready-to-copy files. Every client starts the command `ogun` with the argument `mcp`.

| Client | Configuration |
|---|---|
| Claude Code | `claude mcp add ogun -- ogun mcp`, the project `.mcp.json`, or the plugin in `integrations/mcp/claude-code/plugin` |
| Claude Desktop | `mcpServers.ogun` in `claude_desktop_config.json` |
| Codex CLI | In `~/.codex/config.toml`: `[mcp_servers.ogun]` with `command = "ogun"` and `args = ["mcp"]` |
| Cursor | In `.cursor/mcp.json`: `{"mcpServers": {"ogun": {"command": "ogun", "args": ["mcp"]}}}` |
| VS Code (agent mode) | In `.vscode/mcp.json`: `{"servers": {"ogun": {"type": "stdio", "command": "ogun", "args": ["mcp"]}}}` |
| Windsurf | `mcpServers.ogun` in `~/.codeium/windsurf/mcp_config.json` |
| Zed | `context_servers.ogun` in `settings.json` |

## 4. The CLI JSON contract

Integrations depend on these outputs. Their members are only ever added, never renamed or removed, within a major version.

| Command | `--json` output |
|---|---|
| `ogun validate FILE` | `{"diagnostics": [...], "workflow_hash"?, "summary"?}`; exit 0 if valid, else 1 |
| `ogun hash FILE` | `{"workflow_hash": "..."}` |
| `ogun diff OLD NEW` | `{"from_hash", "to_hash", "diff"}`; exit 1 when the new version needs approval |
| `ogun partitions FILE [--from --to] [--store]` | `{"to_run": [...], "up_to_date": [...]}` |

Exit status 2 means a usage or I/O error.

## 5. Data platforms

### 5.1 Shape and credentials

Orchestrators call Ogun; Ogun never drives them (ADR 0012). An integration reaches `ogund` through the local API of O-15, using `ogun-client` (`integrations/python/ogun-client`, the O-15 client in Python), or through the CLI JSON contract of section 4. Remote workers wait for the server's HTTP bridge (ADR 0010).

1. Each integration uses its own local credential of kind `agent`, which a person creates once on a terminal: `ogun credentials add <name> --kind agent` (`airflow`, `dagster`, `prefect`, `kestra`).
2. The platform's configuration (an Airflow connection, a Dagster resource, a Prefect block, a Kestra flow) names the credential file. It MUST NOT hold the token.
3. `ogun-client` accepts only `agent` credentials by default, and it refuses to send `approvals.decide`, the `credentials.*`, `secrets.*` and `approvers.*` methods, `edges.enroll` and `daemon.shutdown`. `ogund` refuses them for agents in any case (O-15 section 6).
4. `ogun-client` finds the endpoint exactly as `ogun-api` does (O-15 section 2: `OGUN_HOME`, the platform default, the BLAKE3-derived socket fallback and pipe name) and performs the handshake of O-15 section 4.1. Its tests check both against vectors produced by the Rust crate.

### 5.2 Integrations

| Platform | Package | Shape |
|---|---|---|
| Airflow | `airflow-provider-ogun` (`integrations/airflow`). The `apache-airflow-providers-` prefix is left to providers the Apache Airflow community releases. | `OgunHook` (connection type `ogun`); `OgunRunOperator` (start a run, bound to `workflow_hash` when given, for the latest partition, keys or a range; wait, defer to `OgunRunTrigger`, or return at once; push `ogun_run_id` and the artifact digests to XCom; cancel on kill); `OgunRunSensor` (run succeeded); `OgunPartitionSensor` (partition `done`). |
| Dagster | `dagster-ogun` (`integrations/dagster`) | `OgunResource`, and `ogun_workflow_asset`: one asset per workflow, partitioned like it (`time` day and month, `static`, and `window` given a first window), materialised by starting a run of the partition or, for a single-run backfill, of the range; the materialisation records the run id, the workflow hash and the artifact digests. Window partitions are checked, not run, because the stream seals them. |
| Prefect | `prefect-ogun` (`integrations/prefect`) | The `OgunConnection` block; tasks `ogun_run`, `start_ogun_run`, `wait_for_ogun_run`, `ogun_partition_state`. |
| Kestra | `integrations/kestra` | A flow blueprint: a Commands task pre-flights the namespace's workflow file with `ogun validate --json` and `ogun hash --json` against the pinned hash; a Python Script task starts the run with `ogun-client` and emits the run id, state and digests as outputs. Both use the Process task runner on the `ogund` host. A plugin task `io.verne.ogun.Run` is planned. |
| dbt | | The `dbt` code operator (O-03). |
| Warehouses | | Parquet staged in object storage, then the native load, idempotent by partition and revision. |
| Lineage | | OpenLineage run events with facets `ogun_artifact` (digest, revision) and `ogun_window` (window key, revision). |
| Grafana | | A read-only query bridge over sealed windows and rollups. |

### 5.3 Idempotency

Every integration derives its idempotency key from the orchestrator's own identity of the attempt, so a retry re-attaches to the run it started instead of starting a second one:

| Platform | Key |
|---|---|
| Airflow | `airflow:<dag_id>:<task_id>:<run_id>:<map_index>`; a retry after that run failed or was cancelled appends the try number |
| Dagster | `dagster:<run_id>:<asset key>:<partitions>` |
| Prefect | `prefect:<flow_run_id>:<task_run_id>:<workflow>` |
| Kestra | `kestra:<namespace>:<flow>:<execution>:<task>` |

A key that is longer than 128 characters or not printable ASCII is replaced by `k1-` and the BLAKE3 hex digest of its text.

### 5.4 Approvals

Runs started by an integration bind to a workflow hash. If the hash is not approved, the run waits in `waiting_for_user` (O-01) and the integration waits with it, until a person approves on the Ogun CLI or in Studio, or the integration's own timeout passes. The integration cannot change that.

## 6. Open issues

1. The language server uses the built-in operator catalogue; installed operator manifests (O-03) and a "diff against the approved version" lens need `ogund`, which the server deliberately does not contact.
2. MCP `listChanged`: tools that appear when `ogund` starts after `ogun mcp` are listed only when the credential file already exists.
3. Dagster Pipes, for reporting artifacts from inside Ogun runs, waits for the operator ABI (O-03) to carry a Pipes context.
4. The Kestra plugin task, and Go and Java clients of O-15.
5. Remote workers: integrations reach `ogund` only on its host until the server bridge (ADR 0010, O-13) exists.
6. Publishing: VS Code Marketplace and Open VSX, PyPI for the Python packages, the MCP registry and the Claude Code plugin marketplace.
