# Roadmap

Targets, not promises. Hephaestus follows Zeus: no runtime code until Zeus reaches its phase 1 exit criteria (crash-and-resume download with matching SHA-256; handoff success of at least 85 percent on the 200-URL test set). Until then, the work here is contracts and research. They are cheap, and they decide whether the platform is reachable at all ([`docs/VISION.md`](docs/VISION.md) section 15).

## Phase H0: contracts (now, alongside Zeus specs 05 to 08)

**Prerequisite:** the maintainer accepts or amends ADR 0002 to 0005 here and Zeus ADR 0011.

Specs to write in `docs/spec/`, in this order. The first four are the contracts the vision calls decisive.

| Spec | Title | Depends on |
|---|---|---|
| H-00 | Conventions delta: Hephaestus id prefixes, inherited Zeus 00 rules | Zeus 00 |
| H-01 | Domain model and run state machine (table-driven, like Zeus 01): Source, Artifact ref, Asset, Operation, Run, Node instance, Workflow, Agent, Policy, Environment, Evaluator; mapping of Zeus job states | Zeus 01, Zeus 13 |
| H-02 | Workflow IR v0: DAG, operator references, routing predicates, partitions, workflow hash, plan diff, embedding of Zeus acquisition plans | Zeus 08 (rescoped) |
| H-03 | Operator ABI v0: manifest (name, version, inputs, outputs, determinism, sandbox profile), invocation protocol (process with JSON over stdio first; WASI components considered), error mapping, version pinning | H-01 |
| H-04 | Scheduling: triggers (time, event, data, agent, condition), partitions, backfills, concurrency and budgets | H-01, H-02 |
| H-05 | Lineage and run records: artifact edges, agent-run records, cost ledger, trajectories, OpenLineage export | H-01, Zeus 04, Zeus 13 |
| H-06 | Benchmark profile: Environment, Evaluator, isolation, cache modes, adapters (Harbor, ADE-bench, Spider2-DBT) | H-01, H-05 |

Zeus-side contracts that Hephaestus needs (tracked in Zeus ADR 0011): spec 13 (artifacts and content-addressed store), spec 05 (`artifacts.*` methods, a credential for `hephd`), spec 08 (acquisition-only scope).

Research, in `docs/research/`, using the same document structure as Zeus (front matter, TL;DR, "Implications for Hephaestus", open questions, references). Each document stays `draft` until it has been verified.

| # | Topic | Absorbs Zeus planned doc |
|---|---|---|
| HR1 | Durable execution and asset orchestration: Temporal, Restate, DBOS, Dagster assets, Prefect, Kestra, Flyte, Hamilton; what "durable" costs in a single-user local daemon | 26 workflow-engine-dsl-and-durable-execution |
| HR2 | Operator sandboxing and ABI: subprocess JSON protocols, WASI 0.2 and 0.3 components, containers, Windows job objects and AppContainer, Linux Landlock and seccomp, macOS sandbox | 17 conversion-parsing-and-sandboxed-pipelines |
| HR3 | Agent benchmark harnesses and trajectory formats: Harbor and Terminal-Bench 2.0, ADE-bench, Spider 2.0, Inspect, SWE-bench harness, multi-benchmark harnesses for data agents; harness-optimization research | part of 23 testing-verification-and-benchmarking-methodology |
| HR4 | Lineage, dataset formats and data quality: OpenLineage, W3C PROV, Croissant, Parquet, Arrow, Lance, Iceberg, Delta, DuckLake; Great Expectations, Soda, pandera | 27 llm-data-mining-pipelines-and-dataset-formats (Zeus keeps 18 provenance standards) |

Exit: H-00 to H-03 accepted, with JSON Schemas and examples that a validator checks (same tooling as `zeus/docs/spec/tools`).

## Phase H1: runtime MVP (after Zeus phase 1)

- Cargo workspace (`hephaestus-*` crates; binaries `heph`, `hephd`): IR types and validator, durable run store, run state machine, Zeus API client.
- Core operators: `zeus.acquire`, `archive.extract`, `hash`, `dedupe`, `manifest`, `file.copy`, `file.move`. One external adapter: DuckDB.
- CLI: `heph plan`, `heph run`, `heph status`, `heph backfill`. A small MCP tool set.
- Exit: the NOAA-style backfill (partitioned acquisition over a date range) survives a `kill -9` of `hephd` and of `zeusd` mid-run, and resumes only the incomplete partitions.

## Phase H2: data and model operators

- Python, SQL and dbt adapters with sandbox profiles. Code-operator approvals through the decided inbox (ADR 0002 Q1).
- LLM operators (`llm.extract`, `llm.classify`, `embedding.generate`) on the provider ladder (ADR 0002 Q3).
- Assets, partitions and schedules with time, event and data triggers. The Python SDK and YAML authoring compile to IR.

## Phase H3: benchmark profile

- Environments in containers, adapters for Spider2-DBT and ADE-bench, trajectories and replay, matrix runs, the metrics in the vision.
- Uses these benchmarks as integration tests for H1 and H2.

## Phase H4: Studio

- Tauri GUI as a human client of both daemons (ADR 0002 Q2): Runs, Workflows, Artifacts, Sources, Datasets, Agents, Sessions, Schedules, Connections; the approval inbox; lineage and trajectory timelines.
- A2A peer (ADR 0002 Q4).

## Not planned

The list in ADR 0004, plus everything Zeus excludes: captcha solving, TLS fingerprint spoofing, reading browser cookie databases, DRM circumvention, telemetry.
