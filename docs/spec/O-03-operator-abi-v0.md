# O-03. Operator ABI v0

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-01, O-02 |
| Decided by | Proposed ADR 0003 decision 3; proposed ADR 0004 |
| Implemented by | Catalogue (section 7) in `ogun-ir`; manifests and invocation are not implemented yet |

## 1. Purpose

An operator is one unit of work in a workflow. This spec defines what an operator declares (its manifest), how `ogund` invokes it, and the built-in catalogue that IR v0 validates against.

## 2. Kinds

| Kind | Runs | User code | Examples |
|---|---|---|---|
| `core` | Inside `ogund`, in Rust | no | `acquire`, `hash`, `dedupe`, `manifest` |
| `adapter` | Sandboxed child process | no | `duckdb`, `pdf.extract`, `tesseract` |
| `code` | Sandboxed child process | yes, by `code.blake3` | `python`, `sql`, `dbt`, `shell` |
| `model` | Sandboxed child process | no; calls a model through Ogun's provider ladder | `llm.extract`, `embedding.generate` |

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

## 4. Invocation: `jsonl-stdio/0`

1. `ogund` starts the command in its sandbox with a clean environment, the working directory set to a fresh scratch directory, and no inherited handles except stdin, stdout and stderr.
2. It writes one JSON line to stdin: `{"type": "start", "node_instance": "ni_...", "config": {...}, "inputs": [{"port", "path", "blake3", "media_type"}], "secrets": {...}}`. Secrets appear only as resolved values for the `secret_ref` names the node uses, and only on stdin.
3. The operator writes JSON lines to stdout: `progress` (`done`, `total`), `output` (`port`, `path` inside scratch, `media_type`, optional `metadata`), `checkpoint` (an opaque object at most 64 KiB, given back on restart) and finally exactly one of `result` (`ok`) or `error` (an envelope, O-00 section 5).
4. stderr is captured as diagnostics, size-limited and redacted before it is stored.
5. `ogund` ingests every declared output into the artifact store and commits them before the node instance moves to `succeeded`.
6. An exit without `result` or `error` is `op.crashed`; exceeding `policy.timeout_ms` is `op.timeout`.

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
