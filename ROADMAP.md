# Roadmap

Targets, not promises. Ogun is built on the `zeus-core` engine (ADR 0002). Its runtime starts when that engine survives a crash and resumes. The Zeus daemon smoke test showed this on 2026-10-18: killed after 4 MiB of an 8 MiB transfer, restarted, resumed with the original validator, verified (proposed [ADR 0006](docs/adr/0006-zeus-boundary-update.md)). Fault injection at every crash point is still open Zeus phase 1 work, which Ogun tracks but does not wait for. It does not wait for the Zeus extension or the consumer app either. Contracts still come first, because they decide whether the platform is reachable at all ([`docs/VISION.md`](docs/VISION.md) section 15), so O0 and the first O1 crates proceed together.

## Phase O0: contracts (now; Zeus specs 00 to 14 are in Draft 1)

ADRs 0001 to 0012 are accepted (0003 to 0012 on 2026-10-23).

Status on 2026-10-23: O-00 to O-03 are Draft 1 in `docs/spec/`, with a JSON Schema for the workflow IR and an example the test suite validates. O-05 (lineage and run records), O-06 (benchmark profile) and O-08 (session bridge) are Draft 1 with their crates.

Specs to write in `docs/spec/`, in this order. The first four are the contracts the vision calls decisive.

| Spec | Title | Depends on |
|---|---|---|
| O-00 | Conventions delta: Ogun id prefixes, inherited Zeus 00 rules, Ogun's local API transport (Zeus ADR 0002 and 0010 pattern) | Zeus 00 |
| O-01 | Domain model and run state machine (table-driven, like Zeus 01): Source, Artifact, Asset, Operation, Run, Node instance, Workflow, Agent, Policy, Environment, Evaluator; mapping of engine job states | Zeus 01, Zeus 13 |
| O-02 | Workflow IR v0: DAG, operator references, routing predicates, partitions, workflow hash, plan diff; `acquire` configuration in the Zeus plan vocabulary | Zeus 08 |
| O-03 | Operator ABI v0: manifest (name, version, inputs, outputs, determinism, sandbox profile), invocation protocol (a process with JSON over stdio first; WASI components considered), error mapping, version pinning | O-01 |
| O-04 | Scheduling: triggers (time, event, data, agent, condition), partitions, backfills, concurrency and budgets | O-01, O-02 |
| O-05 | Lineage and run records: artifact edges, agent-run records, cost ledger, trajectories, OpenLineage export (Draft 1, `ogun-lineage`) | O-01, Zeus 04, Zeus 13 |
| O-06 | Benchmark profile: Environment, Evaluator, isolation, cache modes, adapters (Harbor, ADE-bench, Spider2-DBT) (Draft 1, `ogun-bench`) | O-01, O-05 |
| O-07 | Approvals and trust surfaces in Ogun (Zeus ADR 0007 rules applied to Ogun's domain) | Zeus 07 |
| O-08 | Session bridge: `service` credential, linking, write-root registration, importing Zeus jobs with provenance (Draft 1, `ogun-zeus-bridge`) | Zeus 05 |

Zeus-side work Ogun needs (tracked in Zeus ADR 0011):
- **Done: spec 13.** The artifact and content-addressed store is Draft 1 and implemented as the `zeus-core` crate `zeus-artifact`.
- **Done: spec 05.** The `service` client kind (section 7.1), its rules (7.6), `files.roots.register` (9.6) and the linking flow (15.2) are specified and implemented.
- **Open: register Ogun's id prefixes** (O-00) in Zeus spec 00 section 5.1, through a Zeus pull request.
- **Open: `zeus-core` public API review:** the engine now has two first-party consumers.
- **Open: crate moves.** `zeus-llm`, `zeus-a2a` and the pure plan-IR parts of `zeus-workflow` are still in the app repository; they move to `zeus-core` when Ogun first needs them (Zeus spec 08 section 12 describes the shape).
- **Done in Ogun: a daemon-API client without app crates.** `zeus-api` depends on app crates, so ADR 0006 has Ogun write its own spec 05 client: `ogun-zeus-bridge` (O-08), which reproduces the spec 05 handshake test vector.

Research goes in `docs/research/`, using the same document structure as Zeus (front matter, TL;DR, "Implications for Ogun", open questions, references). Every document stays `draft` until it is verified.

| # | Topic | Overlaps Zeus doc (not yet written) |
|---|---|---|
| OR1 | Durable execution and asset orchestration: Temporal, Restate, DBOS, Dagster assets, Prefect, Kestra, Flyte, Hamilton; what "durable" costs in a single-user local daemon | 26 workflow-engine-dsl-and-durable-execution |
| OR2 | Operator sandboxing and ABI: subprocess JSON protocols, WASI 0.2 and 0.3 components, containers, Windows job objects and AppContainer, Linux Landlock and seccomp, macOS sandbox | 17 conversion-parsing-and-sandboxed-pipelines |
| OR3 | Agent benchmark harnesses and trajectory formats: Harbor and Terminal-Bench 2.0, ADE-bench, Spider 2.0, Inspect, SWE-bench harness, multi-benchmark harnesses for data agents; harness-optimization research | 23 testing-verification-and-benchmarking-methodology |
| OR4 | Lineage, dataset formats and data quality: OpenLineage, W3C PROV, Croissant, Parquet, Arrow, Lance, Iceberg, Delta, DuckLake; Great Expectations, Soda, pandera | 27 llm-data-mining-pipelines-and-dataset-formats, 18 provenance standards |

Zeus research docs 17, 18, 23, 26 and 27 are planned but not written yet (the Zeus corpus has 01, 02, 03, 05, 06, 07, 08, 10, 12, 15 and 28, all draft). OR1 to OR4 cite them once they exist and go deeper for developer workflows.

Exit: O-00 to O-03 accepted, with JSON Schemas and examples that a validator checks (same tooling as `zeus/docs/spec/tools`).

## Phase O1: runtime MVP (started 2026-10-23)

- Cargo workspace (`ogun-*` crates; binaries `ogun` and `ogund`): IR types and validator, durable run store, run state machine, embedded `zeus-core` engine with the null session provider.
  - Done: `ogun-types`, `ogun-ir` (types, validator, workflow hash, plan diff), `ogun-run` (table-driven run and node state machine), `ogun-sched` (cron, partitions, backfill plans, budgets), `ogun-store` (durable run store with a kill-and-recover test), and `ogun validate`, `hash`, `diff`, `partitions` and `next`.
  - Done: `ogun-api` (O-15) and `ogund` (`ogun-daemon`): the local API, executor, scheduler, stream driver and approvals (O-07), with `ogun daemon`, `put`, `run`, `backfill`, `partition-status`, `approve` and the other client commands. See `docs/IMPLEMENTATION_STATUS.md`.
  - Next: the core operators behind `ogun-ops`, and the O1 exit test.
- Core operators: `acquire`, `archive.extract`, `hash`, `dedupe`, `manifest`, `file.copy`, `file.move`. One external adapter: DuckDB.
  - Done (`ogun-ops`, `ogun-op-shims`): the operator registry; every core operator of O-03 section 7 (with `acquire` on the zeus-core engine and a no-session provider, and a robots-aware same-origin `crawl`); the `jsonl-stdio/0` runner with manifests, `config_schema` checks, secrets on stdin only and redacted stderr; the bubblewrap and `sandbox-exec` sandboxes; shims for DuckDB, SQL, dbt, shell, Poppler, Tesseract, PaddleOCR, ffmpeg, pandoc, yt-dlp, rclone, git and Hugging Face. The Smart Air Python operators run unchanged.
  - Next: wiring into `ogund`; a loopback test hook for `acquire` and `crawl`; Windows sandboxing.
- CLI: `ogun plan`, `ogun run`, `ogun status`, `ogun backfill`. A small MCP tool set.
- Exit: a NOAA-style backfill (partitioned acquisition over a date range) survives a `kill -9` of `ogund` mid-run and resumes only the incomplete partitions. **Met on 2026-10-23** for a 121-partition backfill of core operators (`crates/ogun-daemon/tests/exit.rs`); the same run with `acquire` over a real source is the next check.

## Streams, edge, platforms and extensions (added 2026-10-23)

New requirements: continuous sensor and telemetry streams, edge devices, every desktop and mobile platform plus a self-hosted server, and extensions for editors, coding agents and data platforms. Research draft OR5 and ADRs 0007 to 0012 cover them.

- Done (pure logic, tested):
  - `ogun-stream`: event-time windows, per-device watermarks, sealing, revisions and the `late` output.
  - `ogun-edge`: the durable ingest log with backpressure, and the have/want sync protocol.
  - IR stream members, `ogun mcp` (read-only tools), and the VS Code extension.
  - `ogun lsp` (`crates/ogun-lsp`): validator diagnostics, hover, completion, go-to-definition, symbols and workflow hash; the VS Code extension uses it when found.
  - `ogun mcp` daemon-backed tools with an `agent` credential, client configurations in `integrations/mcp`, the Python O-15 client, and Airflow, Dagster, Prefect and Kestra integrations.
  - The O-12 platform matrix, with the core crates type-checked for Android and iOS.
  - `ogun-ingest`, the ingest bridge: MQTT (TLS, QoS 1, persistent session, PUBACK after commit), HTTP push, LoRaWAN (ChirpStack v4, The Things Stack v3), Modbus TCP and RTU-over-TCP, and file tail, each with its own durable log and a `streams.push` forwarder. Unit-tested; not yet run against `ogund`.
  - `ogun-sync`: edge keys, signed offers, the edge client (`ogun-sync keygen`, `once`, `run`), and the hub-side offer verification and chunk assembly. Unit-tested; the hub handlers and relay routes are not wired yet.
- Next, in order:
  1. The CI matrix passing (blocked on the account's Actions billing).
  2. An acceptance run of `ogun-ingest` against `ogund` (broker restart, `ogund` restart, full log under each `when_full` policy), after the O1 exit test.
  3. Done: range-compressed partition status (`partitions.status`, `ogun partition-status`).
  4. Done: `ogun lsp` (`ogun-lsp`), with the VS Code client.
  5. Server mode without remote approvals. Started 2026-10-23: O-13 is Draft 1 and `ogun-server` (OIDC, workspaces, roles, API tokens, Studio-web REST and events, MCP over HTTP, the sync relay) builds; deployment assets are in `deploy/`. It waits for `ogund` to run end to end.
  6. Done: edge-to-hub sync on a hub (`sync.offer`, `sync.chunk` in `ogund` through `ogun-sync`, the relay routes in `ogun-server`); a soak test over a link that drops is next.
  7. Remote approvals with WebAuthn, after a security review. The passkey registration and decision relay are implemented behind `remote_approvals` (off by default); the external review (O-13 section 12) is open.
  8. The Android companion and gateway. Code in `apps/studio` (companion over a hub's REST routes; gateway with MQTT, LAN HTTP push, the ingest log and signed O-11 sync); not yet built for Android or run on a device.
  9. The iOS companion. Code in `apps/studio` (read-only hub link); not yet built with Xcode.
- The O1 exit test gains a stream twin: kill `ogund` during a window, restart, and the re-seal gives the same digest with no lost or duplicated record. **Met on 2026-10-23** (`crates/ogun-daemon/tests/exit.rs`).

## Phase O2: data and model operators

- Python, SQL and dbt adapters with sandbox profiles; code-operator approvals on Ogun's trust surfaces (O-07).
- Ogun's own LLM provider ladder; LLM operators (`llm.extract`, `llm.classify`, `embedding.generate`). Built: `ogun-llm` (ladder, Anthropic, OpenAI, OpenAI-compatible, Ollama and Gemini clients, spend ledger) and `ogun-op-llm` (all eight model operators, O-03 section 8); the `ogund` runner side (injection, ledger writes) is next.
- Assets, partitions and schedules with time, event and data triggers. The Python SDK and YAML authoring compile to IR.
- Session bridge (O-08): the client crate `ogun-zeus-bridge` is done (handshake, root registration, submission, status, import, tested against a fake `zeusd`); wiring it into `ogund` and acceptance against a real `zeusd` with the extension handoff remain.

## Phase O3: benchmark profile

- Environments in containers; adapters for Spider2-DBT and ADE-bench; trajectories and replay; matrix runs; the metrics in the vision.
  - Done (2026-10-23): `ogun-bench` with docker, podman and local environments, command evaluators, agent runs with chained trajectories, the result cache, Harbor, Spider 2.0-DBT and ADE-bench adapters (formats unverified), matrices with JSON, Markdown and OpenLineage results, `ogun-bench replay`. The local runtime is tested end to end; the container path has not run in CI yet.
  - Next: verify the adapters against upstream (OR3), parallel and resumable matrices, harness gain and tool ablation.
- These benchmarks double as integration tests for O1 and O2.

## Phase O4: Studio and A2A

- Ogun Studio (Tauri): Runs, Workflows, Artifacts, Sources, Datasets, Agents, Schedules, Benchmarks, Connections; the approval inbox; lineage and trajectory timelines.
  - Started 2026-10-23 in `apps/studio`: overview, workflows (normal form, diagnostics, DAG, versions, import), runs with live node states, partitions and backfill plans, the approval inbox (Studio evidence on the desktop, passkeys on Studio-web), artifacts with previews and lineage, streams, edges and settings. Waits for `ogund` and `ogun-server` for end-to-end runs.
- Ogun's A2A agent card and bridge endpoint, off by default.

## Not planned

The list in ADR 0004, plus everything Zeus excludes: captcha solving, TLS fingerprint spoofing, reading browser cookie databases, DRM circumvention, telemetry.
