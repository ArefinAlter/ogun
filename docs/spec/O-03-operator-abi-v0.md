# O-03. Operator ABI v0

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-01, O-02 |
| Decided by | ADR 0003 decision 3; ADR 0004 |
| Implemented by | Catalogue (section 7) in `ogun-ir`; registry, core operators, manifests, `jsonl-stdio/0` runner and sandbox (sections 2 to 6) in `ogun-ops`; tool shims in `ogun-op-shims` |

## 1. Purpose

An operator is one unit of work in a workflow. This spec defines what an operator declares (its manifest), how `ogund` invokes it, and the built-in catalogue that IR v0 validates against.

## 2. Kinds

| Kind | Runs | User code | Examples |
|---|---|---|---|
| `core` | Inside `ogund`, in Rust | no | `acquire`, `hash`, `dedupe`, `manifest` |
| `adapter` | Sandboxed child process | no | `duckdb`, `pdf.extract`, `tesseract` |
| `code` | Sandboxed child process | yes, by `code.blake3` | `python`, `sql`, `dbt`, `shell` |
| `model` | Sandboxed child process | no; calls a model through Ogun's provider ladder | `llm.extract`, `embedding.generate` |
| `source` | The `ogun-ingest` bridge process (O-10) | no | `stream.mqtt`, `stream.http` |

Only `core` operators run in the daemon. Everything else is a child process; user code never runs in `ogund`.

## 3. Manifest

Every non-core operator ships a manifest. `ogund` refuses to run an operator whose manifest is missing or invalid, and refuses any action the manifest does not declare (`op.sandbox_violation`).

```json
{
  "name": "pdf.extract",
  "version": "1.2.0",
  "kind": "adapter",
  "inputs":  [{ "name": "pdf",  "media_type": "application/pdf" }],
  "outputs": [{ "name": "text", "media_type": "text/markdown" }, { "name": "tables", "media_type": "application/vnd.apache.parquet" }],
  "determinism": "pure",
  "sandbox": { "network": "none", "fs": ["inputs:ro", "scratch:rw"], "memory_bytes": 1073741824, "cpu_ms": 600000 },
  "invocation": { "protocol": "jsonl-stdio/0", "command": ["ogun-op-pdf"] },
  "config_schema": { "type": "object" }
}
```

| Member | Meaning |
|---|---|
| `name` | Dotted name, as in O-02 section 6.1. |
| `version` | Semantic version. The major version is what IR references pin. |
| `kind` | Section 2. |
| `inputs`, `outputs` | Named ports with media types. IR inputs reference output names. |
| `determinism` | `pure` (same inputs and config give the same outputs), `seeded` (pure given a recorded seed) or `nondeterministic` (network, models, clocks). Decides caching and replay. |
| `sandbox` | Network (`none`, `allowlist` with hosts, `any`), filesystem grants, memory and CPU limits. Enforced, not advisory. |
| `invocation` | Protocol and command. |
| `config_schema` | JSON Schema for `config`; checked before dispatch. |

The manifest's BLAKE3 over its JCS form is recorded in the lineage of every artifact the operator produces, together with the code hash for `code` operators.

Two optional members extend the table:

| Member | Meaning |
|---|---|
| `tools` | External programs the command drives: `[{"any_of": ["hf", "huggingface-cli"], "optional": false}]`. A missing required tool is `op.tool_missing`; the tools' directories are bound read-only into the sandbox. |
| `code` | `{"bundle": "file"}` (default) or `{"bundle": "archive"}`: the verified code file is a zip or tar archive, extracted safely into the scratch directory before start. |

`invocation.command` may use the placeholders `{code}`, `{code_dir}`, `{scratch}` and `{manifest_dir}`. File system grants are `inputs:ro`, `scratch:rw` (required), `code:ro` and `workspace:ro` (the daemon's read-only workspace directory). Every adapter, code and model operator of section 7 has a manifest compiled into `ogund`. An installed manifest (`<name>.manifest.json` in an operator directory) with the same name and major version replaces it; it MUST NOT change the kind or drop a catalogue output (`op.manifest_invalid`). This settles O-02 open issue 3.

A `network` grant of `allowlist` cannot be enforced by the OS sandboxes of this version (they isolate the network namespace or share it); such a manifest is refused with `op.sandbox_unavailable` rather than run with more network than it declares.

## 4. Invocation: `jsonl-stdio/0`

1. `ogund` starts the command in its sandbox with a clean environment, the working directory set to a fresh scratch directory, and no inherited handles except stdin, stdout and stderr.
2. It writes one JSON line to stdin: `{"type": "start", "node_instance": "ni_...", "config": {...}, "inputs": [{"port", "path", "blake3", "media_type"}], "secrets": {...}}`. Secrets appear only as resolved values for the `secret_ref` names the node uses, and only on stdin.
3. The operator writes JSON lines to stdout: `progress` (`done`, `total`), `output` (`port`, `path` inside scratch, `media_type`, optional `metadata`), `checkpoint` (an opaque object at most 64 KiB, given back on restart) and finally exactly one of `result` (`ok`) or `error` (an envelope, O-00 section 5).
4. stderr is captured as diagnostics, size-limited and redacted before it is stored.
5. `ogund` ingests every declared output into the artifact store and commits them before the node instance moves to `succeeded`.
6. An exit without `result` or `error` is `op.crashed`; exceeding `policy.timeout_ms` is `op.timeout`.

Details of version 0:

- **Start line.** Besides the members above it carries `protocol`, `op`, `run_id`, `node_id`, `partition`, `window` (`start_ms`, `end_ms`, when the partition has one), `attempt`, `deadline_ms`, `run_start_ms`, `scratch`, `workspace` (with the `workspace:ro` grant), `code` (`path`, `blake3`, and `dir` for an archive bundle) and `checkpoint`. An input's `port` is the id of the producing node (`sales` for `sales.tables`) and `ref` the full reference. `config` keeps its `{"secret_ref"}` objects; `secrets` maps exactly the names the node uses to their values. Run-context placeholders (`{partition}`, `{partition.year}`, `{run.date}` and the rest of the closed set in `ogun_ops::context`) are expanded in `config` before the operator sees it.
- **More lines.** `usage` (`provider`, `model`, `input_tokens`, `output_tokens`, `cost_usd_micro`) from model operators, and `log` (`message`). Unknown line types are ignored. A line longer than 1 MiB, a line that is not JSON, or a checkpoint over 64 KiB is `op.protocol_violation`. An output on a port the manifest does not declare, or a path outside the scratch directory after resolving links, is `op.sandbox_violation`. Output metadata holds no floating-point numbers; the runner turns any into integers or decimal text.
- **Errors.** A child's envelope keeps a registered `op.*`, `llm.*`, `secret.*` or `artifact.*` code (and `run.budget_exhausted`); any other code becomes `op.operator_failed`, or `op.operator_transient` when the child marked a transient class retryable, with the child's code in `details.operator_code`.
- **stderr.** At most 64 KiB per attempt, lines cut at 2 KiB; every resolved secret value (raw and percent-encoded) and every URL query is removed before a line becomes a run log entry.
- **Sandbox.** Linux: bubblewrap (new namespaces, network unshared unless granted, read-only system directories, inputs, code, workspace and tools, read-write scratch, private `/tmp`, no home directory, `--die-with-parent`, `--new-session`, capabilities dropped). macOS: `sandbox-exec` with a generated deny-by-default profile. `memory_bytes` and `cpu_ms` become address-space and CPU-time limits. Where no sandbox is available (Windows in this version), child operators are refused with `op.sandbox_unavailable` unless the daemon runs a development profile that allows unsandboxed operators; every such attempt is logged. Cancellation kills the child's process group.

WASI components are under evaluation as a second protocol (research OR2).

## 5. Errors and retries

An operator classifies its failure with an envelope. The class decides between `retryable_error` and `permanent_error` in O-01: retryable classes are retried up to `policy.retries`; `policy_denied`, `invalid_request`, `integrity` after one retry, and `permanent` are not.

## 6. Version pinning

IR references pin the major version. The run record stores the exact version, manifest hash and, for `code` operators, the code hash. A new minor or patch version is used by new runs only; a run in progress keeps the version it started with.

## 7. Built-in catalogue (IR v0)

The validator checks references against this catalogue. Every entry is major version 1.

| Operator | Kind | Outputs |
|---|---|---|
| `acquire` | core | `artifacts` |
| `zeus.session_acquire` | core | `artifacts` |
| `crawl` | core | `urls`, `documents` |
| `file.copy`, `file.move`, `archive.extract`, `hash`, `partition`, `map`, `join_metadata` | core | `artifacts` |
| `dedupe` | core | `artifacts`, `duplicates` |
| `filter` | core | `artifacts`, `rejected` |
| `manifest` | core | `manifest` |
| `duckdb` | adapter | `tables`, `artifacts` |
| `pdf.extract` | adapter | `text`, `tables` |
| `tesseract`, `paddleocr` | adapter | `text` |
| `ffmpeg`, `pandoc`, `yt_dlp`, `rclone`, `git`, `hf` | adapter | `artifacts` |
| `python` | code | `artifacts`, `rows` |
| `polars`, `sql` | code | `tables` |
| `dbt` | code | `artifacts`, `results` |
| `shell` | code | `artifacts` |
| `llm.extract`, `llm.enrich` | model | `rows` |
| `llm.classify` | model | `labels` |
| `llm.map`, `llm.rename` | model | `artifacts` |
| `llm.filter` | model | `artifacts`, `rejected` |
| `llm.schema` | model | `schema` |
| `embedding.generate` | model | `vectors` |
| `window.rollup` | core | `artifacts` |
| `stream.mqtt`, `stream.http`, `stream.lorawan`, `stream.modbus`, `stream.file_tail` | source | `window` |
