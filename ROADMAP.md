# Roadmap

Targets, not promises. Ogun is built on the `zeus-core` engine (ADR 0002). Its runtime starts when that engine survives a crash and resumes. The Zeus daemon smoke test showed this on 2026-10-18: killed after 4 MiB of an 8 MiB transfer, restarted, resumed with the original validator, verified (proposed [ADR 0006](docs/adr/0006-zeus-boundary-update.md)). Fault injection at every crash point is still open Zeus phase 1 work, which Ogun tracks but does not wait for. It does not wait for the Zeus extension or the consumer app either. Contracts still come first, because they decide whether the platform is reachable at all ([`docs/VISION.md`](docs/VISION.md) section 15), so O0 and the first O1 crates proceed together.

## Phase O0: contracts (now; Zeus specs 00 to 14 are in Draft 1)

ADR 0002 (product boundary) is accepted. ADRs 0003 to 0006 are proposed; the maintainer accepts or amends them. O-02 and O-03 are drafted against proposed ADR 0003 and stay Draft until it is accepted.

Status on 2026-10-23: O-00 to O-03 are Draft 1 in `docs/spec/`, with a JSON Schema for the workflow IR and an example the test suite validates.

Specs to write in `docs/spec/`, in this order. The first four are the contracts the vision calls decisive.

| Spec | Title | Depends on |
|---|---|---|
| O-00 | Conventions delta: Ogun id prefixes, inherited Zeus 00 rules, Ogun's local API transport (Zeus ADR 0002 and 0010 pattern) | Zeus 00 |
| O-01 | Domain model and run state machine (table-driven, like Zeus 01): Source, Artifact, Asset, Operation, Run, Node instance, Workflow, Agent, Policy, Environment, Evaluator; mapping of engine job states | Zeus 01, Zeus 13 |
| O-02 | Workflow IR v0: DAG, operator references, routing predicates, partitions, workflow hash, plan diff; `acquire` configuration in the Zeus plan vocabulary | Zeus 08 |
| O-03 | Operator ABI v0: manifest (name, version, inputs, outputs, determinism, sandbox profile), invocation protocol (a process with JSON over stdio first; WASI components considered), error mapping, version pinning | O-01 |
| O-04 | Scheduling: triggers (time, event, data, agent, condition), partitions, backfills, concurrency and budgets | O-01, O-02 |
| O-05 | Lineage and run records: artifact edges, agent-run records, cost ledger, trajectories, OpenLineage export | O-01, Zeus 04, Zeus 13 |
| O-06 | Benchmark profile: Environment, Evaluator, isolation, cache modes, adapters (Harbor, ADE-bench, Spider2-DBT) | O-01, O-05 |
| O-07 | Approvals and trust surfaces in Ogun (Zeus ADR 0007 rules applied to Ogun's domain) | Zeus 07 |
| O-08 | Session bridge: `service` credential, linking, write-root registration, importing Zeus jobs with provenance | Zeus 05 |

Zeus-side work Ogun needs (tracked in Zeus ADR 0011):
- **Done: spec 13.** The artifact and content-addressed store is Draft 1 and implemented as the `zeus-core` crate `zeus-artifact`.
- **Done: spec 05.** The `service` client kind (section 7.1), its rules (7.6), `files.roots.register` (9.6) and the linking flow (15.2) are specified and implemented.
- **Open: register Ogun's id prefixes** (O-00) in Zeus spec 00 section 5.1, through a Zeus pull request.
- **Open: `zeus-core` public API review:** the engine now has two first-party consumers.
- **Open: crate moves.** `zeus-llm`, `zeus-a2a` and the pure plan-IR parts of `zeus-workflow` are still in the app repository; they move to `zeus-core` when Ogun first needs them (Zeus spec 08 section 12 describes the shape).
- **Open: a daemon-API client without app crates.** `zeus-api` depends on app crates, so proposed ADR 0006 has Ogun write its own spec 05 client.

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
  - Next: `ogund` with the embedded engine and its scheduling loop, and the core operators.
- Core operators: `acquire`, `archive.extract`, `hash`, `dedupe`, `manifest`, `file.copy`, `file.move`. One external adapter: DuckDB.
- CLI: `ogun plan`, `ogun run`, `ogun status`, `ogun backfill`. A small MCP tool set.
- Exit: a NOAA-style backfill (partitioned acquisition over a date range) survives a `kill -9` of `ogund` mid-run and resumes only the incomplete partitions.

## Phase O2: data and model operators

- Python, SQL and dbt adapters with sandbox profiles; code-operator approvals on Ogun's trust surfaces (O-07).
- Ogun's own LLM provider ladder; LLM operators (`llm.extract`, `llm.classify`, `embedding.generate`).
- Assets, partitions and schedules with time, event and data triggers. The Python SDK and YAML authoring compile to IR.
- Session bridge (O-08), once Zeus phase 1 has shipped the extension handoff.

## Phase O3: benchmark profile

- Environments in containers; adapters for Spider2-DBT and ADE-bench; trajectories and replay; matrix runs; the metrics in the vision.
- These benchmarks double as integration tests for O1 and O2.

## Phase O4: Studio and A2A

- Ogun Studio (Tauri): Runs, Workflows, Artifacts, Sources, Datasets, Agents, Schedules, Benchmarks, Connections; the approval inbox; lineage and trajectory timelines.
- Ogun's A2A agent card and bridge endpoint, off by default.

## Not planned

The list in ADR 0004, plus everything Zeus excludes: captcha solving, TLS fingerprint spoofing, reading browser cookie databases, DRM circumvention, telemetry.
