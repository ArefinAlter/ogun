# ADR 0005: Benchmark runs are a run profile

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-06 |
| Decided by | Maintainer, 2026-10-23 ("accept for all") |

## Context

Ogun's run model (durable runs, isolated workspaces, tools, model adapters, artifacts, provenance, cost accounting) is most of what a harness for data and terminal agents needs. Spider 2.0-DBT (68 DuckDB and dbt tasks), ADE-bench (dbt Labs; modelled on Terminal-Bench; DuckDB and Snowflake variants) and Terminal-Bench 2.0 (Harbor task format and harness) sit in Ogun's domain. A separate benchmark system would duplicate the runtime. Product behaviour, on the other hand (auto-repair, retries, caches, memory), contaminates measurements.

## Decision

1. **Model.** `Run = Workflow + Environment + Agent + Policy + Inputs + Evaluator`. Environment and Evaluator are first-class objects. A benchmark is a controlled collection of runs. There is no separate benchmark architecture.
2. **The benchmark profile disables help.** No automatic repair, no retries beyond what the benchmark defines, no help from other agents, fresh memory, session and workspace, and an explicit `cache_mode` (`disabled`, `benchmark_provided`, `cold`, `warm`).
3. **Isolation by default.** Each run gets an immutable environment (base image, fixture, task repository, agent binary, tool policy) in a container. MicroVMs come later. Network is denied unless the benchmark permits it. `/workspace` is read-write, `/benchmark` read-only, the rest hidden. The only secret is model credentials. There is **no access** to the user's Ogun or Zeus artifact stores, to `zeusd` or to browser sessions.
4. **Adopt existing formats.** Tasks come in through adapters for existing formats: the Harbor task format, ADE-bench, Spider 2.0-DBT. Official evaluators run unmodified, and their version is recorded. Ogun does not define a competing task format.
5. **Trajectories.** Every run records an append-only, replayable event stream (model requests and responses, tool calls, commands, file reads and patches, database state hashes) plus the environment digest, task version, agent and model versions, and prompt and tool-definition hashes.
6. **Metrics beyond pass rate.** Cost and tokens per success, intervention rate, tool and edit efficiency, recovery metrics, harness gain and tool ablation ([`../VISION.md`](../VISION.md) section 12).

## Consequences

- Benchmark runs need a container runtime. On Windows development machines that means Docker or WSL 2. This is the first Ogun feature with a hard platform dependency.
- The benchmark profile does not depend on the `zeus-core` engine. It is the only Ogun slice that could run before the engine works. That is a temptation, and a scope risk.
- Spider2-DBT and ADE-bench double as integration tests for the run, environment, evaluator and trajectory model.

## Alternatives considered

- **A dedicated benchmark product or repository**: deferred. Split it out later if the profile grows its own audience.
- **Contributing to an existing harness instead**: still possible per benchmark. Ogun's distinct value is cross-benchmark matrices, harness ablations, lineage and cost accounting, not "can run the tasks". Harbor and multi-benchmark harnesses for data agents already exist and must be surveyed (research OR3) before any implementation.
