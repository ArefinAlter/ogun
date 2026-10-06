# Roadmap

Targets, not promises. Ogun is built on the `zeus-core` engine (ADR 0002). Its runtime starts when that engine passes its crash-and-resume test: kill the process mid-download, restart, and get a matching SHA-256 (Zeus segment I3). It does not wait for the Zeus extension or the consumer app. Until then the work here is contracts and research. They are cheap, and they decide whether the platform is reachable at all ([`docs/VISION.md`](docs/VISION.md) section 15).

## Phase O0: contracts (now, alongside Zeus specs 05 to 08)

ADR 0002 (product boundary) is accepted. ADRs 0003 to 0005 are proposed; the maintainer accepts or amends them before the specs that depend on them are written.

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
- **Spec 13:** the artifact and content-addressed store format as a `zeus-core` library.
- **Spec 05:** the `service` client kind, write-root registration for service clients, a linking flow.
- **`zeus-core` public API review:** the engine now has two first-party consumers.
- **Crate moves:** `zeus-llm`, `zeus-a2a` and the pure parts of `zeus-workflow` move to `zeus-core` when Ogun first needs them.

Research goes in `docs/research/`, using the same document structure as Zeus (front matter, TL;DR, "Implications for Ogun", open questions, references). Every document stays `draft` until it is verified.

| # | Topic | Overlaps Zeus planned doc |
|---|---|---|
| OR1 | Durable execution and asset orchestration: Temporal, Restate, DBOS, Dagster assets, Prefect, Kestra, Flyte, Hamilton; what "durable" costs in a single-user local daemon | 26 workflow-engine-dsl-and-durable-execution |
| OR2 | Operator sandboxing and ABI: subprocess JSON protocols, WASI 0.2 and 0.3 components, containers, Windows job objects and AppContainer, Linux Landlock and seccomp, macOS sandbox | 17 conversion-parsing-and-sandboxed-pipelines |
| OR3 | Agent benchmark harnesses and trajectory formats: Harbor and Terminal-Bench 2.0, ADE-bench, Spider 2.0, Inspect, SWE-bench harness, multi-benchmark harnesses for data agents; harness-optimization research | 23 testing-verification-and-benchmarking-methodology |
| OR4 | Lineage, dataset formats and data quality: OpenLineage, W3C PROV, Croissant, Parquet, Arrow, Lance, Iceberg, Delta, DuckLake; Great Expectations, Soda, pandera | 27 llm-data-mining-pipelines-and-dataset-formats, 18 provenance standards |

Zeus still writes docs 17, 18, 23, 26 and 27 for the consumer app. Ogun's versions go deeper for developer workflows and cite the Zeus docs instead of repeating them.

Exit: O-00 to O-03 accepted, with JSON Schemas and examples that a validator checks (same tooling as `zeus/docs/spec/tools`).

## Phase O1: runtime MVP (after the engine passes crash-and-resume)

- Cargo workspace (`ogun-*` crates; binaries `ogun` and `ogund`): IR types and validator, durable run store, run state machine, embedded `zeus-core` engine with the null session provider.
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
