# ADR 0004: Integrate, do not replace

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-06 |

## Context

The vision touches a dozen mature categories: orchestrators, ELT, scraping, dataframes, SQL engines, OCR, vector databases, file sync. Orchestration in particular is crowded and consolidating: Prefect announced its acquisition of Dagster Labs on 2026-07-13, with Dagster continuing as separate open-source software. A small team cannot equal any of these products. It can own their intersection: authenticated acquisition plus artifacts plus the orchestration around them.

## Decision

1. **Not built:** a dataframe library, a SQL engine, a browser, an OCR engine, a vector database, a foundation model, a full Airflow or dbt replacement, captcha or anti-bot circumvention, a generic RPA platform.
2. **External tools are operators behind adapters**, invoked as sandboxed processes with declared inputs and outputs: DuckDB, Polars (through Python), dbt, ffmpeg, pandoc, Tesseract or PaddleOCR, yt-dlp, rclone, git, Hugging Face tools. Ogun pins and records each tool's version in the lineage.
3. **Standalone, but embeddable.** Ogun has its own durable scheduler so it can run acquisition workflows alone. It is also callable *from* other orchestrators through its CLI and API (Prefect task, Dagster resource, Airflow operator, Kestra plugin, Temporal activity, GitHub Action), and from agents through MCP and A2A. The first integrations go in that direction; Ogun does not drive other orchestrators.
4. **Lineage interoperates.** Run and lineage events are exportable as OpenLineage. Artifact provenance is exportable through Zeus's manifest exports (W3C PROV, Croissant; Zeus spec 04).

## Consequences

- Adapter quality (error mapping, version pinning, sandbox profiles) matters more than breadth of native operators.
- Users need those tools installed, or Ogun provisions them in an environment. That interacts with the benchmark profile (ADR 0005).

## Alternatives considered

- **Native Rust implementations of common transforms**: only for small, hot, safety-relevant operators (hash, dedupe, archive extraction, manifest).
- **Building on an existing orchestrator** (Dagster or Prefect as the runtime): rejected for the core, because they are Python-first servers. Their run model cannot see the engine's job and session states, and the local-first, single-binary property would be lost. They remain integration targets.
