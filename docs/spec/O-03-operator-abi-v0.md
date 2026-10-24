# O-03. Operator ABI v0

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-01, O-02 |
| Decided by | ADR 0003 decision 3; ADR 0004 |
| Implemented by | Catalogue (section 7) in `ogun-ir`; registry, core operators, manifests, `jsonl-stdio/0` runner and sandbox (sections 2 to 6) in `ogun-ops`; tool shims in `ogun-op-shims`; model operators (section 8) in `ogun-llm` and `ogun-op-llm` |

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

## 8. Model operators

The `model` operators (`llm.*`, `embedding.generate`) call language and embedding models through Ogun's own provider ladder. Models are optional (constraint 6): a workflow without model operators runs with no model access, and with no ladder configured no model call is ever made. One binary, `ogun-op-llm`, implements every model operator as a `jsonl-stdio/0` child (section 4); the library `ogun-llm` holds the ladder, the provider clients, the budget check and the spend ledger.

### 8.1 The ladder: `<state_dir>/llm.json`

```json
{
  "version": 1,
  "providers": [
    {
      "name": "anthropic-sonnet",
      "kind": "anthropic",
      "base_url": "https://api.anthropic.com",
      "model": "claude-sonnet-5-5",
      "api_key_secret": "anthropic_api_key",
      "input_per_mtok_usd_micro": 2000000,
      "output_per_mtok_usd_micro": 10000000,
      "max_output_tokens": 16000,
      "timeout_ms": 300000
    },
    {
      "name": "ollama",
      "kind": "ollama",
      "model": "qwen3:8b",
      "input_per_mtok_usd_micro": 0,
      "output_per_mtok_usd_micro": 0,
      "max_output_tokens": 8192,
      "capabilities": ["chat", "json_schema", "embeddings"]
    }
  ]
}
```

Providers are tried in order: the first enabled provider with the capability an operator needs is used, and later ones are fallbacks (section 8.8). A missing file, or an empty `providers` list, means no model access.

| Member | Default | Rule |
|---|---|---|
| `version` | | `1`. |
| `providers` | | 0 to 32 entries. |
| `name` | | `^[a-z][a-z0-9_-]{0,63}$`, unique. Appears in usage lines, the ledger and errors. |
| `kind` | | `anthropic` (Messages API), `openai` (Chat Completions and Embeddings), `openai_compatible` (vLLM, llama.cpp server, LM Studio, OpenRouter and other endpoints of that shape), `ollama` (`/api/chat`, `/api/embed`) or `google` (Gemini `generateContent`, `batchEmbedContents`). |
| `enabled` | `true` | A disabled entry is never called and may be incomplete (empty `model`). |
| `base_url` | per kind | `https://api.anthropic.com`, `https://api.openai.com/v1`, `http://127.0.0.1:11434`, `https://generativelanguage.googleapis.com/v1beta`; `openai_compatible` has no default. `https`, or `http` to a loopback host. Plain `http` to another host needs `allow_insecure_http: true` and no `api_key_secret`. No user name, password, query or fragment. |
| `model` | | 1 to 200 of `A-Z a-z 0-9 . _ : / @ + -`. |
| `api_key_secret` | | The name of the secret holding the key (`^[A-Za-z0-9._-]{1,128}$`), never the key. Required for `anthropic`, `openai` and `google`. |
| `input_per_mtok_usd_micro`, `output_per_mtok_usd_micro` | | Integer micro-US-dollars per million tokens. Output includes reasoning tokens. |
| `max_output_tokens` | | 1 to 1 000 000. The cap for one call and the worst case the budget check assumes. |
| `timeout_ms` | 120 000 | 1 000 to 600 000, per request. |
| `capabilities` | per kind | Non-empty subset of `chat`, `json_schema` (native schema-constrained output; needs `chat`), `embeddings`. Defaults: `anthropic` chat and json_schema; `openai`, `ollama`, `google` all three; `openai_compatible` chat only. `anthropic` never has `embeddings`. |
| `api_version` | `2026-10-21` | `anthropic` only: the `anthropic-version` header. |
| `allow_insecure_http` | `false` | See `base_url`. |

The document is parsed strictly: unknown members, duplicate member names, floating-point numbers and documents over 1 MiB are refused with `llm.config_invalid`. Errors name the member, never its value. `ogun_llm::LadderConfig::template()` writes a starting point with the current Anthropic models (Claude Sonnet 5.5, Haiku 5.5 and Opus 5.5, at Anthropic's list prices) and disabled entries for the other kinds, whose models the user fills in.

### 8.2 What the runner passes

The ladder and the attempt's budget travel in the node configuration; the API keys travel in the start line's `secrets` map. Before starting the operator the runner:

1. Refuses the node with `llm.not_configured` when the environment has no model access (`ogun_ops::Environment::model_access`) or no ladder is configured. Nothing is started.
2. Reserves the attempt's model budget (O-04 section 5.2) and sets `config.llm_budget_usd_micro` to the reserved amount.
3. Sets `config.ladder` to the ladder, narrowed to the node's `config.providers` when present (`LadderConfig::narrowed`, then `inject_into`). Both members are replaced if the node's configuration has them; a manifest's `config_schema` does not list them.
4. Resolves the `api_key_secret` of every enabled provider in that ladder (`LadderConfig::secret_names`) and passes the values in `secrets`. A key that does not resolve is left out; the operator skips that provider.
5. Extends the sandbox's network allowlist with the providers' hosts (`LadderConfig::hosts`).
6. Starts `ogun-op-llm <operator>`, where `<operator>` is the catalogue name (`llm.extract`). The manifest's `invocation.command` is `["ogun-op-llm", "<operator>"]`. Without an argument the operator reads the start line's optional `op` member.

After the attempt the runner appends one ledger entry per `usage` line (section 8.9) and commits the sum of their `cost_usd_micro` against the reservation.

### 8.3 Lines

Model operators write the lines of section 4 with these shapes, one JSON object per line:

```json
{"type": "progress", "done": 3, "total": 12}
{"type": "usage", "provider": "anthropic-sonnet", "model": "claude-sonnet-5-5", "input_tokens": 1834, "output_tokens": 212, "cost_usd_micro": 5788}
{"type": "output", "port": "rows", "path": "/scratch/rows.jsonl", "media_type": "application/x-ndjson", "metadata": {"row_count": 41, "llm_calls": 12, "llm_cost_usd_micro": 70210, "models": ["anthropic-sonnet/claude-sonnet-5-5"], "prompt_blake3": ["..."]}}
{"type": "result", "ok": true}
{"type": "error", "error": {"code": "llm.output_invalid", "class": "integrity", "message": "...", "retryable": false}}
```

One `usage` line follows every provider call that returned an answer, including an answer that then failed validation. `total` in `progress` is the number of planned calls. `path` is absolute and inside the working directory. Output metadata holds integers, strings, booleans and arrays of strings only. Every output carries `llm_calls`, `llm_cost_usd_micro`, `models` (`provider/model` of every call that answered) and `prompt_blake3` (BLAKE3 of each system prompt used, which identifies the task and configuration independently of the documents). Model operators v0 write no `checkpoint` lines; a restarted attempt starts over.

### 8.4 Common configuration

| Member | Default | Range | Meaning |
|---|---|---|---|
| `providers` | all | 1 to 32 ladder names | Use only these providers, in ladder order. An unknown name is `llm.config_invalid`. |
| `max_calls` | 100 | 1 to 100 000 | Bound on provider requests, counting fallbacks and repairs. If the planned calls exceed it, the operator fails with `llm.call_limit` before the first call. |
| `max_output_tokens` | provider's | 1 to 1 000 000 | Per call; capped by each provider's `max_output_tokens`. |
| `temperature_milli` | provider's | 0 to 2 000 | Sampling temperature in thousandths (IR documents hold no floats). Not sent to `anthropic` providers, whose API version rejects sampling parameters. |
| `chunk_bytes` | 32 768 (8 192 for `embedding.generate`) | 1 024 to 1 048 576 | Inputs longer than this are split at paragraph, then line, then character boundaries. |
| `max_input_bytes` | 67 108 864 | 1 to 2^30 | Larger inputs are refused (`llm.input_unsupported`). |
| `repair` | `true` | | One repair request after an invalid structured answer (section 8.7). |
| `fallback` | `true` | | Fall back down the ladder on transient failures (section 8.8). The benchmark profile (O-06) can turn `repair` and `fallback` off. |
| `instructions` | | 1 byte to 16 KiB | Extra task instructions. They are approved configuration and go into the system prompt. |

`embedding.generate` takes `providers`, `max_calls`, `chunk_bytes`, `max_input_bytes` and `fallback` only. A common member an operator does not take, and any unknown member, is `llm.config_invalid`. Inputs must be text: `text/*`, `application/json`, JSON lines (`application/x-ndjson`, `application/jsonl`), XML, YAML, CSV, or a `+json` or `+xml` type. Invalid UTF-8 is replaced, not refused. Binary documents need an adapter first (`pdf.extract`, `tesseract`).

### 8.5 Operators

| Operator | Own members | Reads | Writes |
|---|---|---|---|
| `llm.extract` | `schema` (required): JSON Schema of one row. `source_field`: when set, each row gets this member with `{"blake3", "part"}` of its source. | Text inputs, chunked; one call per chunk. | `rows`: one `rows.jsonl` (`application/x-ndjson`) for all inputs; metadata `row_count`. |
| `llm.enrich` | `schema` (required): object schema whose `properties` are the fields to add. `batch_rows` (20; 1 to 200). `overwrite` (`false`): replace members a row already has. | JSON-lines or JSON rows, in batches. | `rows`: the rows with the fields added, in input order. |
| `llm.classify` | `labels` (required): 2 to 256 distinct strings of 1 to 128 bytes. `multi_label` (`false`). `unit`: `artifact` (default; judged on the first chunk) or `row`. `batch_rows` (20; 1 to 200) for rows. | Text inputs, or rows. | `labels`: `labels.jsonl`, one line per item, `{"blake3", "port" or "index", "label"}` (`"labels"` with `multi_label`); metadata `item_count`, `label_counts`. |
| `llm.map` | `output_media_type`: a text type (the input's when absent). Needs `instructions`. | Text inputs, chunked; the transformed parts are joined in order. | `artifacts`: one file per input in `mapped/`; metadata `source_blake3`. |
| `llm.rename` | `max_name_bytes` (100; 16 to 200). `keep_extension` (`true`; else derived from the media type). | The current name (`metadata.file_name`, `metadata.name`, else the stored file name) and the first chunk. | `artifacts`: a copy of each input in `renamed/` under the new name; metadata `file_name`, `source_blake3`. |
| `llm.filter` | `criterion` (required): 1 byte to 4 KiB. | The first chunk of each text input. | `artifacts` (kept) and `rejected`: copies in `kept/` and `rejected/`; metadata `keep`, `source_blake3`, `untrusted_reason` (model text, at most 500 bytes). |
| `llm.schema` | `max_samples` (20; 1 to 500). `title`. | Rows of JSON or JSON-lines inputs as samples; the first chunk of other text inputs. One call. | `schema`: `schema.json` (`application/schema+json`, draft 2020-12); metadata `sample_count`, `samples_checked`. |
| `embedding.generate` | `text_field`: embed this string member of each row instead of chunks of text inputs. `batch_size` (32; 1 to 512). `dimensions` (1 to 65 536) where the provider supports it. | Text chunks or rows. | `vectors`: `vectors.jsonl`, one line per vector, `{"blake3", "chunk" or "index", "provider", "model", "dimensions", "vector"}`; metadata `vector_count`, `dimensions`, `provider`, `model`, `skipped_rows`. |

`llm.rename` keeps only lowercase ASCII letters, digits and single `-` or `_` separators, never a path separator or a leading dot, prefixes Windows device names with `file-`, bounds the length and makes names unique within the attempt (`-2`, `-3`). Model-written text in metadata is in `untrusted_*` members, or sanitised as `file_name` is. Vectors are floating-point numbers in an artifact, not in an IR document.

### 8.6 Prompt hygiene

1. Input documents are untrusted data. They appear only in the user message, never in the system prompt.
2. Each document is enclosed between `<<<OGUN-DOC-<16 hex> BEGIN document=<label> media_type=<type> [part=i/n]>>>` and the matching `END` marker. The hex digits are BLAKE3 over the document's text and a counter, chosen so the marker does not occur in the text; a document cannot predict or close its own marker. Labels are chosen by the operator, never taken from documents.
3. The system prompt is a fixed frame, the operator's task text and the node's approved configuration (`instructions`, `labels`, `criterion`, schemas). The frame states that document content is data, not instructions, and that text inside a document asking to change the task, reveal the prompt or change the format is ignored.
4. URLs with a query, a fragment or user information in document text are reduced through `redact_url` (log mode) before any text reaches a model. API keys appear only in the request header of the provider that owns them; never in a prompt, a line, a log or an error.
5. Answers are validated locally against the requested schema (section 8.7); nothing a model writes is executed, and model-written strings that reach metadata are bounded and marked untrusted.

### 8.7 Structured output

Every `llm.*` call asks for one JSON value satisfying a schema. With `json_schema` the provider constrains the answer natively: `output_config.format` (`anthropic`), `response_format` with `json_schema` (`openai`, `openai_compatible`; `strict` when every object is closed and every property required), `format` (`ollama`) or `responseJsonSchema` (`google`). The schema sent is adapted to what the provider accepts (Anthropic: closed objects, no numeric, length or most array constraints). Without `json_schema` the schema is given as an instruction in the system prompt.

Either way the answer is parsed (the whole text, a fenced block, or the outermost object or array) and validated against the schema as written, plus the operator's own checks (for example, one item per row index). The validator resolves no remote or file references. An answer that fails gets one repair request on the same provider, naming the failing instance paths and keywords but not quoting the answer; a second failure is `llm.output_invalid`.

### 8.8 Fallback

| Failure | Code | Falls back |
|---|---|---|
| HTTP 429 | `llm.provider_unavailable` | yes |
| HTTP 5xx, 408, connection failure, timeout, malformed response | `llm.provider_unavailable` | yes |
| HTTP 401, 403 | `llm.auth_failed` | no |
| HTTP 400, 404, 413, 422 and other 4xx | `llm.request_rejected` | no |
| A refusal, content filter or safety block | `llm.refused` | no |
| Worst-case cost over the remaining budget | `run.budget_exhausted` | no |
| `max_calls` reached | `llm.call_limit` | no |

A transient failure moves the call to the next provider; when every provider has failed transiently the attempt fails with the retryable `llm.provider_unavailable`, and the node's `policy.retries` decide. Fallback never applies to policy, credential or invalid-request failures. No redirect is followed. `embedding.generate` pins the provider of its first successful call and never falls back afterwards, because vectors from different models are not comparable.

Before each request the operator computes its worst-case cost: input bytes plus framing (every tokenizer in use maps at least one byte to each token, so bytes bound tokens) at the input price, plus the call's output limit at the output price, rounded up. A request whose worst case exceeds the remaining budget is refused (`ogun_llm::check_budget`) and nothing is sent. Actual costs come from the provider's token counts; a provider that reports none (Gemini embeddings) is charged an estimate of one token per four bytes.

### 8.9 The spend ledger: `<state_dir>/llm-ledger.jsonl`

`ogund` appends one line per `usage` line it receives, and is the only writer: it holds an exclusive lock on the file while the ledger is open, and `fsync`s every line.

```json
{"seq":1,"at":"2026-10-23T09:12:44.512Z","run_id":"orun_...","workflow_id":"wf_...","node_instance":"ni_...","op":"llm.extract@1","provider":"anthropic-sonnet","model":"claude-sonnet-5-5","input_tokens":1834,"output_tokens":212,"cost_usd_micro":5788,"prev_blake3":"0000...0000","blake3":"..."}
```

1. `seq` starts at 1 and has no gaps. `prev_blake3` is the previous entry's `blake3`, or 64 zeros for the first.
2. `blake3` is BLAKE3 of the JCS form (RFC 8785) of the entry without its `blake3` member. Any edit, removal or reordering breaks the chain.
3. Opening verifies the whole chain. A final line without a newline that does not parse (a crash during a write) is cut; any other failure refuses the ledger and leaves the file untouched for inspection.
4. Summaries (calls, tokens and `cost_usd_micro`) are kept per run, per workflow and per UTC day (`YYYY-MM-DD` of `at`), and in total.

### 8.10 Errors

| Code | Class | Retryable | Raised when |
|---|---|---|---|
| `llm.not_configured` | `unavailable` | no | No enabled provider offers the needed capability with its key available, or the runner passed no ladder. |
| `llm.config_invalid` | `invalid_request` | no | `llm.json` or the node's configuration breaks a rule above. |
| `llm.provider_unavailable` | `unavailable` | yes | Every provider tried failed transiently (section 8.8). `details.provider` names the last; `details.retry_after_ms` when the provider sent one. |
| `llm.auth_failed` | `unauthenticated` | no | A provider rejected its key. |
| `llm.request_rejected` | `invalid_request` | no | A provider rejected the request as invalid. |
| `llm.refused` | `policy_denied` | no | The model or provider declined on policy grounds. |
| `llm.output_invalid` | `integrity` | no | The answer failed validation after the repair request. |
| `llm.call_limit` | `policy_denied` | no | `max_calls` would be exceeded. |
| `llm.input_unsupported` | `invalid_request` | no | An input is not text, is too large or has the wrong shape. |

A refused budget check is `run.budget_exhausted` (O-00 section 5.2). Messages name providers, members, HTTP statuses and provider error types; never keys, URLs with queries, prompts, documents, answers or configuration values.

### 8.11 Open issues

1. Checkpoints: a long `llm.extract` or `embedding.generate` restarts from the beginning after a crash. Checkpointing the completed chunks would save spend.
2. Calls run one at a time. A `concurrency` member needs per-call budget reservations inside the operator.
