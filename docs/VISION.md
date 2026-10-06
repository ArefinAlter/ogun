# Hephaestus vision

Status: direction, not a contract. Pre-alpha, 2026-10-06. Decisions are recorded in [`adr/`](adr/); normative contracts will live in `spec/` (none written yet). This document distills the product discussion of 2026-10-06. That discussion called the whole platform "Haul". Haul's acquisition layer is now **Zeus** (sibling repositories `zeus`, `zeus-core`, `zeus-extension`), and the platform built on top of it is **Hephaestus**.

## 1. Thesis

Zeus reliably lands authenticated, messy bytes on disk. Hephaestus turns them into verified, reproducible data assets, on a schedule. Agents do the planning; humans hold the approvals.

The lifecycle Hephaestus owns:

**Acquire → materialize → verify → transform → organize → extract → curate → publish.** Every step is durable, inspectable and reproducible, and agents can operate it.

| System | Thinks in |
|---|---|
| Download manager | `URL → file` |
| Agentic browser | `goal → page interactions` |
| Hephaestus | `goal → durable data operation`: source → acquisition → artifact → transformations → dataset or asset → destination |

The primary object is neither the URL nor the file. It is the **run** and the **artifact graph**. Files are materializations of artifacts. URLs are sources. Downloads are acquisition operations. LLMs are operators. Browser sessions are capabilities. The workflow is the durable specification that connects them.

A request Hephaestus should handle as **one first-class workflow**, not as a pile of downloads, an agent improvising shell commands, or a browser session that has to stay open:

> Every Sunday, find newly published climate-health papers from these six sources, download the PDFs and supplementary data, extract metadata and tables, convert everything to Parquet where possible, deduplicate against my existing library, flag studies using remote sensing, create embeddings and update my DuckDB dataset.

## 2. Zeus and Hephaestus

| Layer | Owner | Contents |
|---|---|---|
| Studio | Hephaestus | GUI, CLI and TUI, agent chat, workflow designer, artifact and lineage explorers, approval inbox (owner open, ADR 0002) |
| Agent control plane | Hephaestus (approvals: Zeus) | Planning, MCP, A2A, model policies, budgets |
| Workflow runtime | Hephaestus | DAGs, schedules, triggers, retries, partitions, backfills |
| Data operators | Hephaestus | Scrape, parse, OCR, convert, SQL, Python, LLM; mostly adapters to existing tools |
| Acquisition runtime | Zeus | HTTP, authenticated browser sessions, in-browser fetch fallback; later S3, Hugging Face, Git, APIs |
| Artifact and file substrate | Zeus | Content-addressed store, hashes, provenance, metadata, safe filesystem operations |

These layers form three concentric products:

1. **Zeus** (layer 1, engine): session-aware acquisition, resumability, browser handoff, artifact store, hashing, provenance, safe file operations, MCP, CLI.
2. **Hephaestus Workflows** (layer 2): DAGs, scheduler, events, partitions, backfills, transformations, Python, SQL and DuckDB, scraping, conversion, LLM operators, data validation.
3. **Hephaestus Studio** (layer 3): the ADE-like environment. GUI, prompt-to-workflow, visual DAG, agent runs, A2A, workspaces, artifact and lineage explorers, dataset browser, human approval inbox, observability.

A later, optional **Hub** (shared workflows, team policies, remote workers, artifact and cache sync, central provenance, workflow registry, enterprise audit) is the only candidate for closed components. Like every paid layer in Zeus ADR 0006, it never sits in the credential path.

The split exists for three reasons: the session moat stays small and auditable; arbitrary code (operators) and model calls never run in the process that holds cookies; and Zeus remains useful on its own. ADR 0002 records the boundary.

## 3. Object model

| Concept | Meaning | Owner |
|---|---|---|
| Source | Something data can be acquired from: HTTP URL, browser page, authenticated origin, API, Git repository, S3 prefix, RSS feed, Hugging Face dataset, database query, local directory, MCP or A2A resource | Zeus (acquisition), Hephaestus (discovery) |
| Artifact | An immutable materialization: `paper.pdf`, `dataset.parquet`, `page.html`, `model.safetensors` | Zeus (identity and store) |
| Asset | A logical object with many versions or materializations | Hephaestus |
| Operation | Something done to one or more artifacts: download, crawl, extract, decompress, convert, OCR, parse, filter, join, embed, classify, rename, move, upload, query, LLM-extract | Hephaestus (acquisition operations delegate to Zeus) |
| Run | One durable execution of a workflow | Hephaestus (Zeus keeps its own acquisition-plan runs) |
| Workflow | A versioned declarative specification | Hephaestus |
| Agent | A human or machine actor that plans or performs operations | Hephaestus |
| Policy | Permissions, budgets, rate limits, approval requirements | Both: Zeus for acquisition and sessions, Hephaestus for compute and model spend |
| Environment | The isolated place a run executes: base image, fixtures, workspace, tool policy | Hephaestus |
| Evaluator | Something that scores a run's outputs | Hephaestus |

An artifact knows its id, content hash, size, MIME type, retrieval time, origin and source, licence, parent artifacts, producing operation, workflow and run, verification state and metadata. Zeus spec 04 already records most of these per file in the provenance manifest. Making them a first-class artifact with a stable id is an extension of that ledger, not a redesign.

An asset groups versions, for example *WHO cholera surveillance dataset*: 2026-09-01, 2026-09-08, 2026-09-15. Dagster's asset-oriented model is the reference to study.

With Environment and Evaluator in the model, a run becomes:

```text
Run = Workflow + Environment + Agent + Policy + Inputs + Evaluator
```

A benchmark is then just a controlled collection of runs (section 12). Once these objects exist, much of the functionality stops being special cases.

## 4. The ADE analogy

Agentic development environments organize work around runs and tasks rather than editor buffers: parallel orchestration, isolated execution, approval gates, persistent state, instrumentation. Each of those concepts translates directly:

| Coding ADE | Hephaestus |
|---|---|
| Repository | Data workspace |
| Git worktree | Isolated run workspace |
| Agent task | Data operation or run |
| Source code | Source data and artifacts |
| Diff | Data and file plan diff |
| Test suite | Validators and data-quality checks |
| Commit | Materialized immutable artifact |
| Pull request | Human approval gate |
| Git history | Provenance and lineage |
| Agent fleet | Workers and specialist agents |
| CI | Scheduled or event-triggered workflow |
| Token usage | Compute, network and model cost ledger |

## 5. Workflow runtime

Hephaestus needs a real durable workflow engine, but a lightweight one. Zeus's scheduler is a weighted-fair download queue; Hephaestus adds a durable run and node state machine above it:

`pending`, `ready`, `running`, `waiting`, `waiting_for_user`, `waiting_for_session`, `retry_scheduled`, `succeeded`, `failed`, `cancelled`, `skipped`.

`waiting_for_user` and `waiting_for_session` map onto Zeus job states `awaiting_approval` and `awaiting_session`. A run holds node instances, artifact edges, checkpoints, retry state, an event log and provenance records. A run must survive a daemon crash, a reboot or a closed browser, because agentic work is long.

**Triggers**, richer than cron:

| Kind | Examples |
|---|---|
| Time | cron, interval |
| Event | new file, browser download failed, filesystem changed, RSS changed, webhook (via a bridge), Git commit, S3 object appeared |
| Data | dataset version changed, new matching item discovered, row count over N |
| Agent | A2A task received, MCP invocation, another agent's task completed |
| Condition | retry when login restored, continue when VPN online, download on Wi-Fi only, process on mains power |

Agents should be able to ask for "run this every six hours, but acquire only files that were not present in the previous successful run" or "keep checking this portal until the October release appears, then download everything and run pipeline X".

**Partitions and backfills** are borrowed from data orchestrators and become powerful when combined with session-aware acquisition. A source like `NOAA/YYYY/MM/DD/*.csv` gets per-partition status (2020-01-01 succeeded, 2020-01-03 failed, and so on) and a command such as:

```text
heph backfill climate-data --from 2020-01-01 --to 2026-10-05
```

## 6. Operators

Hephaestus orchestrates existing tools. It does not reimplement them.

| Kind | Operators |
|---|---|
| Core (Rust) | `zeus.acquire` (delegates to Zeus), `crawl`, `file.copy`, `file.move`, `archive.extract`, `hash`, `dedupe`, `manifest`, `partition`, `filter`, `map`, `join_metadata` |
| External (adapters) | Python, shell, SQL, DuckDB, Polars, dbt, ffmpeg, pandoc, Tesseract or PaddleOCR, yt-dlp, rclone, git, hf-xet |
| LLM | `llm.extract`, `llm.classify`, `llm.map`, `llm.filter`, `llm.rename`, `llm.schema`, `llm.enrich`, `embedding.generate` |

The operator ABI (how an operator declares inputs, outputs, version, sandbox profile and determinism, and how it is invoked) is one of the four contracts to design before implementation (see [`../ROADMAP.md`](../ROADMAP.md)).

## 7. Authoring

The core stays in Rust. ML and data engineers live in Python, so Python is a first-class **authoring** language, not the runtime. Python, YAML and natural language all compile to one workflow IR (ADR 0003):

```text
Prompt → LLM → workflow IR → validator → plan and diff → human approval → runtime
```

Never `prompt → arbitrary autonomous shell session`. That distinction is what gives reproducibility.

```python
from hephaestus import workflow, acquire

@workflow(schedule="0 3 * * *")
def arxiv_pipeline():
    papers = acquire.crawl("https://...", pagination="auto", match="*.pdf")
    text = papers.map("pdf.extract")
    structured = text.map("llm.extract", schema=Paper)
    structured.write.parquet("papers.parquet")
```

```yaml
workflow: arxiv-papers
trigger:
  cron: "0 3 * * *"
steps:
  - id: discover
    uses: crawl
    with: { url: "https://...", pagination: auto }
  - id: acquire
    uses: zeus.acquire
    from: discover.urls
  - id: extract
    uses: pdf.extract
    from: acquire.artifacts
  - id: classify
    uses: llm.extract
    from: extract.text
    with: { schema: paper.json }
  - id: publish
    uses: parquet.write
```

Both are illustrative. The syntax is not decided.

## 8. Models are optional

`download → unzip → CSV → Parquet` runs with no model access at all. AI earns its place in planning, selector discovery, schema inference, classification, semantic extraction, natural-language rules, error diagnosis, workflow modification and data QA. The result is deterministic infrastructure under probabilistic intelligence.

## 9. Provenance and content-addressed storage

Provenance may become more strategically valuable than acquisition itself. Every record of a dataset built by Hephaestus can carry:

```json
{
  "artifact": "art_...",
  "sha256": "...",
  "source_url": "(redacted per Zeus spec 00)",
  "retrieved_at": "2026-10-06T03:00:00.000Z",
  "license": "unknown",
  "workflow_version": "...",
  "session_origin": "https://example.org",
  "parent_artifacts": [],
  "transformations": [
    { "operator": "pdf.extract", "version": "..." },
    { "operator": "llm.extract", "model": "...", "prompt_hash": "..." }
  ]
}
```

That answers the questions serious ML data work asks: which model produced this record, which raw file and which site it came from, whether upstream changed, what was fetched with authenticated access, which transform created this column, and whether dataset version v18 can be reproduced.

Physical blobs are stored by hash (`objects/sha256/ab/cd/abcdef...`), and logical projects reference them. Downloading the same 20 GB model twice does not duplicate storage, and a transformed dataset shares its unchanged parts. The path runs from file-level CAS to chunk-level CAS, then content-defined chunking, then a remote artifact cache (Zeus research doc 06 covers the mathematics). The store is Zeus's (ADR 0002).

## 10. Agents

**Agent runs are first-class objects.** Each records the agent, task, plan, permissions, workspace, tools, inputs, outputs, model, tokens, cost, wall time, network bytes, artifacts created, decisions and approval events. A developer can see:

```text
Claude Code          requested workflow
Hephaestus planner   generated workflow v17
Extractor agent      7,284 documents, $1.38 model cost
Quality agent        rejected 213 records
Zeus                 43.7 GB acquired
```

**MCP** exposes a small, hierarchical surface (workflow create, validate, plan, run, pause, resume, backfill; source inspect and discover; artifact query, lineage, materialize; dataset describe and validate; schedule create; run status, logs, retry; session request through Zeus), plus resources. It does not expose hundreds of tiny tools, because tool schemas cost context (Zeus research doc 10).

**A2A** finally has a reason to exist: Hephaestus is a peer in agent pipelines. A research agent asks it to acquire SEC filings; it hands artifacts to an extraction agent; that agent hands structured records to an analysis agent. Or Codex writes an extractor, Hephaestus runs it over 100,000 files, a QA agent examines the failures, Codex fixes the extractor, and Hephaestus retries only the failed partitions. A2A remains an interface to the runtime, not the runtime.

## 11. Studio

Under this model a GUI is no longer an IDM clone. Main navigation: **Runs, Workflows, Artifacts, Sources, Datasets, Agents, Sessions, Schedules, Connections.** Downloads are one view under Runs.

```text
┌──────────────────────────────────────────────────────────┐
│ Climate Papers, run 184                      ● running   │
├──────────────────────────────────────────────────────────┤
│ Discover ──▶ Acquire ──▶ Extract ──▶ Classify ──▶ Save   │
│   ✓ 481      341/481     219/341       182        ...    │
├────────────────────────────┬─────────────────────────────┤
│ ARTIFACTS                  │ AGENT                       │
│ 481 URLs                   │ "14 PDFs appear scanned.    │
│ 341 PDFs                   │  Enable OCR?"               │
│ 219 extracted documents    │                             │
│ 182 structured rows        │ [Approve]  [Skip]           │
├────────────────────────────┴─────────────────────────────┤
│ Events · Logs · Lineage · Costs · Network                │
└──────────────────────────────────────────────────────────┘
```

The approve button is a human trust surface. It must follow Zeus ADR 0007 (human-only, bound to a request hash).

## 12. Benchmark profile

Hephaestus can serve as a harness for data and terminal agents: Spider 2.0-DBT (68 DuckDB and dbt tasks), ADE-bench (dbt Labs; modelled on Terminal-Bench; dbt repositories with a DuckDB or Snowflake database; a sandbox per task) and Terminal-Bench 2.0 (distributed in the Harbor task format and run with the Harbor harness). These benchmarks sit almost exactly in the Hephaestus domain: repositories, SQL, dbt, datasets, terminals, transformations, generated artifacts and validation.

**Product mode optimizes for completing the task; benchmark mode optimizes for measuring the agent.** Benchmark mode must therefore switch off helpful behaviour that contaminates evaluation: automatic retries and repairs, schema inference, help from other agents, cached artifacts and recovered downloads. Otherwise the measurement is "Hephaestus plus model", not the configuration under test. ADR 0005 records the rules:

- Isolation: immutable environment (base image, fixture, task repository, agent binary, tool policy) in a container, a microVM later. Network denied by default. `/workspace` read-write, `/benchmark` read-only, the rest hidden. Model credentials only. No access to the user's artifact store or to Zeus sessions.
- Cache modes `disabled`, `benchmark_provided`, `cold`, `warm`. Fresh memory, session and workspace unless a longitudinal study asks otherwise.
- The trajectory is a replayable event stream (`task.started`, `model.request`, `model.response`, `tool.shell`, `tool.result`, `file.read`, `file.patch`, and so on), alongside the initial repository hash, environment digest, task version, agent and model versions, prompt and tool-definition hashes, database state before and after, evaluator output, tokens, latency and cost. `heph bench replay <run>` and the Studio timeline render it.

Metrics beyond pass rate:

| Metric | Definition |
|---|---|
| Cost per success | C_s = Σ_i cost_i / number of successful tasks |
| Intervention rate | interventions per run |
| Tool efficiency | useful tool calls / all tool calls (proxies: duplicate, failed and repeated commands, files re-read) |
| Edit efficiency | necessary changed lines / all changed lines (needs task-aware heuristics) |
| Recovery | failure detection rate, mean steps to recovery, recovery success rate |
| Harness gain | HGE(H) = (1/N) Σ_m [Score(m, H) − Score(m, H_baseline)] over N models |
| Tool ablation | Δ_i = Score_full − Score_(full without tool i) |

Harness gain and tool ablation ask a better research question than another leaderboard: how much capability does poor tooling hide?

Adapters stay tiny:

```rust
trait Benchmark {
    fn tasks(&self) -> Vec<Task>;
    async fn prepare(&self, task: &Task, env: &Environment) -> Result<Fixture>;
    async fn evaluate(&self, task: &Task, run: &Run) -> Result<Score>;
}

trait AgentAdapter {
    async fn launch(&self, task: &Task, workspace: &Path, tools: ToolPolicy) -> Result<AgentRun>;
}
```

Hephaestus does not win by "running Spider". Harbor, ADE-bench's own harness and Spider 2.0's agent framework already do that, and multi-benchmark harnesses for data agents are appearing. It wins if researchers can run benchmark families × agents × models × harness configurations × seeds in controlled environments, and get reproducibility, trajectories, lineage, cost accounting, failure taxonomies, harness ablations and cross-benchmark analysis. Spider2-DBT and ADE-bench are also the best **integration tests** for the architecture itself. If the runtime can execute their tasks, capture trajectories, reset environments and run evaluations, the core abstractions hold.

## 13. What Hephaestus will not build

Its own dataframe library, SQL engine, browser, OCR engine, vector database or foundation model; a full Airflow or dbt replacement; autonomous captcha or anti-bot circumvention; a generic RPA platform. Hephaestus integrates these (ADR 0004). Its coherent responsibility is **materializing trustworthy data artifacts from heterogeneous sources, and orchestrating everything required around that materialization.**

## 14. Position

The comparison set is no longer IDM, Gopeed, aria2 and JDownloader. It is an intersection of Gopeed and aria2, Firecrawl and Crawl4AI, dlt and Airbyte, Prefect, Dagster and Kestra, OpenRefine, rclone, MCP and A2A runtimes, and coding-ADE interaction patterns. Hephaestus should not try to equal any of them. Its value is the intersection: **the local-first agentic runtime that turns authenticated, messy web and filesystem sources into verified, reproducible data assets.**

Orchestration is crowded and consolidating. Prefect announced its acquisition of Dagster Labs on 2026-07-13, and Dagster continues as a separate open-source product. So Hephaestus is not "a better Airflow". It contains enough durable orchestration to run acquisition workflows by itself, and it plugs into the orchestrators: as a Prefect task, a Dagster resource, an Airflow operator, a Kestra plugin, a Temporal activity, a GitHub Action, an MCP server and an A2A agent.

Candidate taglines, undecided: "From browser session to reproducible dataset". "A durable data plane for agents". "The acquisition and artifact runtime for AI agents" (the last describes Zeus plus Hephaestus).

## 15. Scope discipline

The original research warned that building the GUI, workflows, A2A, a file manager and a downloader at once would mean catastrophic scope creep. That warning still holds. The answer is a deliberate platform order, Zeus → Hephaestus Workflows → Hephaestus Studio:

1. Prove the one technically unusual part first: session-aware acquisition (Zeus phase 1 exit criteria).
2. Before much implementation, design the four contracts that decide whether Hephaestus can become this platform or gets trapped as a sophisticated downloader: the **workflow IR**, the **artifact and asset model**, the **run state machine** and the **operator ABI**.

## 16. Name

"Hephaestus" is crowded in exactly this space. Ido-Levi/Hephaestus is an open-source, semi-structured agentic workflow framework. amurshak/hephaestus is a workflow pattern for AI coding agents. On crates.io, `hephaestus` is a 2D renderer published in August 2026 and `heph` is an actor framework. Treat the name as a codename until a clearance is done, as Zeus ADR 0006 already requires for "Zeus". Proposed binaries are `heph` (CLI) and `hephd` (daemon); workspace crates are `hephaestus-*` and unpublished.
