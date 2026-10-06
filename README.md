# Hephaestus

Hephaestus is a local-first agentic data environment. It turns authenticated, messy web and filesystem sources into verified, reproducible data assets, on a schedule, and agents can operate it.

> **Status: pre-alpha, contracts phase.** No code yet. This repository holds the vision, the architecture decisions and, soon, the normative specs. Runtime work starts after Zeus reaches its phase 1 exit criteria ([`ROADMAP.md`](ROADMAP.md)). "Hephaestus" is a codename ([`docs/VISION.md`](docs/VISION.md) section 16).

## What it is

| Zeus (sibling project) | Hephaestus (this project) |
|---|---|
| Session-aware acquisition: resumable downloads, browser handoff through an MV3 extension, consent-gated sessions | Workflows over what Zeus acquires: DAGs, schedules, triggers, partitions, backfills |
| Artifact store: content hashes, provenance manifest, safe file operations | Operators: archive, convert, parse, OCR, SQL, DuckDB, dbt, Python, LLM extraction and classification |
| Human-only approvals, the trust surfaces, the one local listener | Assets, datasets, transform lineage, agent-run records, cost ledger |
| MCP tools for acquisition | Studio (an ADE-style GUI), A2A peer, and a benchmark profile for data and terminal agents (Spider 2.0-DBT, ADE-bench) |

Lifecycle: **acquire → materialize → verify → transform → organize → extract → curate → publish.** Every step is durable, inspectable and reproducible.

Hephaestus is a separate process (`hephd`) and a client of `zeusd`. It never holds browser session material, and it never decides approvals ([ADR 0002](docs/adr/0002-separate-process-client-of-zeusd.md)). Workflows are data, not code ([ADR 0003](docs/adr/0003-workflow-ir-is-data.md)). It integrates DuckDB, dbt, Polars, ffmpeg, OCR engines and orchestrators instead of replacing them ([ADR 0004](docs/adr/0004-integrate-do-not-replace.md)). Models are optional: a workflow with no LLM operator runs with no model access at all.

## Repositories

| Repository | Role |
|---|---|
| `hephaestus` (this one) | Workflow runtime, operators, Studio, benchmark profile |
| `zeus` | Zeus app: `zeusd`, `zeus` CLI and TUI, MCP server, native host; normative specs 00 to 12 |
| `zeus-core` | Zeus engine library crates |
| `zeus-extension` | Zeus MV3 browser extension |

## Read next

1. [`docs/VISION.md`](docs/VISION.md): the product, the object model, the benchmark profile.
2. [`docs/adr/`](docs/adr/): the decisions (most are `Proposed`).
3. [`ROADMAP.md`](ROADMAP.md): contracts first, then the runtime.

## Licence

Apache-2.0, like Zeus (Zeus ADR 0006). See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
