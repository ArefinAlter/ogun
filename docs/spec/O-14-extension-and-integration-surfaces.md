# O-14. Extension and integration surfaces

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-02, O-04 |
| Decided by | Proposed ADRs 0011 and 0012 |
| Implemented by | `ogun mcp` and `--json` output in `ogun-cli`; `extensions/vscode` |

## 1. Purpose

This spec defines how editors, coding agents and other data platforms reach Ogun, and what none of them may do: decide an approval, raise a budget, or run user code outside an approved workflow.

## 2. Editors

**VS Code** (`extensions/vscode`) does four things:

- bundles the IR JSON Schema for `*.workflow.json`, for completion and structural checks;
- runs `ogun validate --json` on open and on save;
- places each diagnostic at the member its JSON Pointer names;
- shows the workflow hash in the status bar.

The bundled schema MUST equal `docs/spec/schema/workflow-ir.v0.schema.json`, and a test checks it.

**Other editors** (JetBrains, Zed, Neovim) will use `ogun lsp` (planned): diagnostics, hover with operator outputs, and go-to-definition for input references.

## 3. MCP

### 3.1 Transport

`ogun mcp` speaks MCP over stdio as newline-delimited JSON-RPC 2.0. It supports protocol revisions `2025-11-25`, `2025-06-18` and `2025-03-26`. It answers `initialize` with the client's requested revision when it supports it, and with the newest otherwise.

### 3.2 Tools

Every tool is read-only (`readOnlyHint: true`).

| Tool | Arguments | Result |
|---|---|---|
| `ogun_validate` | `workflow`: IR as JSON text | Diagnostics; for a valid workflow, the hash and a summary |
| `ogun_diff` | `old`, `new` | The plan diff, `needs_approval` and both hashes |
| `ogun_partitions` | `workflow`, optional `from` and `to` | Partition count and the first 100 keys |
| `ogun_operators` | none | The operator catalogue |

A tool failure (an invalid workflow, for example) is a tool result with `isError: true`, so the agent can read and fix it. Missing arguments are JSON-RPC error `-32602`.

### 3.3 Client configuration

| Client | Configuration |
|---|---|
| Claude Code | `claude mcp add ogun -- ogun mcp` |
| Codex CLI | In `~/.codex/config.toml`: `[mcp_servers.ogun]` with `command = "ogun"` and `args = ["mcp"]` |
| Cursor | In `.cursor/mcp.json`: `{"mcpServers": {"ogun": {"command": "ogun", "args": ["mcp"]}}}` |
| VS Code (agent mode) | In `.vscode/mcp.json`: `{"servers": {"ogun": {"type": "stdio", "command": "ogun", "args": ["mcp"]}}}` |
| Windsurf, Zed | The same command and arguments in their MCP settings |

### 3.4 Planned tools

Run, status and backfill tools come with `ogund`. They will start runs only of workflow versions that a person has already approved, and they never approve.

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

All integrations wrap the CLI contract, or the server's HTTP bridge (proposed ADR 0010).

| Platform | Shape |
|---|---|
| Airflow | Provider package `apache-airflow-providers-ogun`: `OgunRunOperator` (start a run of an approved workflow version by hash, wait, push artifact digests to XCom) and `OgunSensor` (partition done). |
| Dagster | A resource using Dagster Pipes, so Ogun artifacts appear as asset materializations with their digests. |
| Prefect | An `ogun_run` task and an `OgunServer` block. |
| Kestra | A plugin task `io.verne.ogun.Run`. |
| dbt | The `dbt` code operator (O-03). |
| Warehouses | Parquet staged in object storage, then the native load, idempotent by partition and revision. |
| Lineage | OpenLineage run events with facets `ogun_artifact` (digest, revision) and `ogun_window` (window key, revision). |
| Grafana | A read-only query bridge over sealed windows and rollups. |

Runs started by an integration bind to a workflow hash. If the hash is not approved, the run waits in `waiting_for_user` (O-01). The integration cannot change that.

## 6. Open issues

1. `ogun lsp` capabilities beyond diagnostics.
2. Publishing: VS Code Marketplace and Open VSX, PyPI for the Python wrappers, and the MCP registry.
