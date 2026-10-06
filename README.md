# Ogun

Ogun is a local-first developer platform for data mining, data engineering and agent benchmarking, built on the Zeus download engine (`zeus-core`). It is for AI, LLM and data engineers who need to turn messy web and filesystem sources into verified, reproducible datasets on a schedule, and to let agents plan and operate that work.

> **Status: pre-alpha, contracts phase.** No code yet. This repository holds the vision, the architecture decisions and, soon, the normative specs. Runtime work starts when the `zeus-core` engine passes its crash-and-resume test ([`ROADMAP.md`](ROADMAP.md)).

## Ogun and Zeus

Two products share one engine ([ADR 0002](docs/adr/0002-separate-product-on-zeus-core.md)):

| | Zeus app | Ogun (this repository) |
|---|---|---|
| For | Consumers and their agents | AI, LLM and data engineers |
| What | Agentic download and file manager | Data mining, data-engineering workflows, agent benchmarks |
| Engine | `zeus-core`, inside `zeusd` | `zeus-core`, embedded in `ogund` |
| Browser sessions | Yes: MV3 extension and native host | Only through the optional Zeus session bridge (`ogund` is a `service` client of `zeusd`) |
| GUI | Zeus GUI | Ogun Studio |
| LLM provider ladder, A2A, approvals | Its own | Its own |

Lifecycle: **acquire → materialize → verify → transform → organize → extract → curate → publish.** Every step is durable, inspectable and reproducible. Workflows are data, not code ([ADR 0003](docs/adr/0003-workflow-ir-is-data.md)). Ogun integrates DuckDB, dbt, Polars, ffmpeg, OCR engines and orchestrators instead of replacing them ([ADR 0004](docs/adr/0004-integrate-do-not-replace.md)). A benchmark is just a run profile for Spider 2.0-DBT, ADE-bench and Terminal-Bench tasks ([ADR 0005](docs/adr/0005-bench-is-a-run-profile.md)). Models are optional: a workflow with no LLM operator runs with no model access at all.

## Repositories

| Repository | Role |
|---|---|
| `ogun` (this one) | Developer platform: workflows, operators, lineage, Studio, benchmark profile |
| `zeus-core` | Shared engine library (transfer, journal, verification, control, filesystem) |
| `zeus` | Zeus consumer app (`zeusd`, CLI and TUI, MCP, native host) and the normative Zeus specs |
| `zeus-extension` | Zeus MV3 browser extension |

## Read next

1. [`docs/VISION.md`](docs/VISION.md): the product, the object model, the benchmark profile.
2. [`docs/adr/`](docs/adr/): the decisions.
3. [`ROADMAP.md`](ROADMAP.md): contracts first, then the runtime.

## Licence

Apache-2.0, like Zeus (Zeus ADR 0006). See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
