# Ogun vision

Status: direction, not a contract. Pre-alpha, 2026-10-06; Zeus references updated 2026-10-23. Decisions are recorded in [`adr/`](adr/); normative contracts will live in `spec/` (none written yet). This document distills the product discussion of 2026-10-06. That discussion called the whole platform "Haul". The maintainer then split it into two products on one engine: **Zeus**, the consumer agentic download and file manager (`zeus`, `zeus-extension`), and **Ogun**, the developer platform described here. Both are built on the shared engine library `zeus-core` (ADR 0002).

## 1. Thesis

Zeus's engine reliably lands messy bytes on disk. Ogun is the developer tool that uses that engine for data mining, data engineering and agent benchmarking. Its users are AI, LLM and data engineers. It turns sources into verified, reproducible data assets on a schedule. Agents do the planning; humans hold the approvals.

The lifecycle Ogun owns:

**Acquire → materialize → verify → transform → organize → extract → curate → publish.** Every step is durable, inspectable and reproducible, and agents can operate it.

| System | Thinks in |
|---|---|
| Download manager | `URL → file` |
| Agentic browser | `goal → page interactions` |
| Ogun | `goal → durable data operation`: source → acquisition → artifact → transformations → dataset or asset → destination |

The primary object is neither the URL nor the file. It is the **run** and the **artifact graph**. Files are materializations of artifacts. URLs are sources. Downloads are acquisition operations. LLMs are operators. Browser sessions are capabilities. The workflow is the durable specification that connects them.

A request Ogun should handle as **one first-class workflow**, not as a pile of downloads, an agent improvising shell commands, or a browser session that has to stay open:

> Every Sunday, find newly published climate-health papers from these six sources, download the PDFs and supplementary data, extract metadata and tables, convert everything to Parquet where possible, deduplicate against my existing library, flag studies using remote sensing, create embeddings and update my DuckDB dataset.

## 2. Zeus and Ogun

```text
              zeus-core (shared engine library)
   transfer · resume · journal · verification · control · fs · artifact store format
              /                                        \
   Zeus app (consumer)                           Ogun (developer platform)
   agentic download and file manager             data mining, data engineering, benchmarks
   zeusd · CLI/TUI · Zeus GUI · MCP              ogund · ogun CLI · Ogun Studio · MCP
   extension + native host (browser sessions)    workflows · operators · lineage · bench
   own LLM ladder · own A2A · own approvals      own LLM ladder · own A2A · own approvals
              \________ optional session bridge ________/
                 (Ogun is a `service` client of zeusd)
```

| Layer | In Ogun | Contents |
|---|---|---|
| Studio | Ogun Studio, `ogun` CLI and TUI | Workflow designer, agent chat, run, artifact, lineage and benchmark views, Ogun's approval inbox |
| Agent control plane | Ogun | Planning, MCP, A2A, Ogun's LLM provider ladder, model policies, budgets |
| Workflow runtime | Ogun | DAGs, schedules, triggers, retries, partitions, backfills |
| Data operators | Ogun | Scrape, parse, OCR, convert, SQL, Python, LLM; mostly adapters to existing tools |
| Acquisition runtime | `zeus-core` engine embedded in `ogund` | HTTP with resume, verification and adaptive control; Ogun's own credentials for APIs, S3 and Hugging Face; browser-session sources only through the Zeus session bridge |
| Artifact and file substrate | `zeus-core` store format, Ogun's own store instance | Content-addressed store, hashes, provenance, metadata, safe filesystem operations |

Ogun ships as two layers on the engine:

1. **Ogun Workflows**: DAGs, scheduler, events, partitions, backfills, transformations, Python, SQL and DuckDB, scraping, conversion, LLM operators, data validation, benchmark profile.
2. **Ogun Studio**: the ADE-like environment. GUI, prompt-to-workflow, visual DAG, agent runs, A2A, workspaces, artifact and lineage explorers, dataset browser, human approval inbox, observability.

The Zeus app stays the consumer product its research describes, with its own GUI, LLM ladder and A2A. A later, optional **Hub** for Ogun (shared workflows, team policies, remote workers, artifact and cache sync, central provenance, workflow registry, enterprise audit) is the only candidate for closed components. Like every paid layer in Zeus ADR 0006, it never sits in the credential path.

Ogun is a separate product rather than a Zeus feature for three reasons. The audiences differ: consumers and their agents versus engineers. Arbitrary code (operators) must never run in the process that holds browser cookies. And each product stays coherent. ADR 0002 records the boundary.

## 3. Object model

| Concept | Meaning | Owner |
|---|---|---|
| Source | Something data can be acquired from: HTTP URL, browser page, authenticated origin, API, Git repository, S3 prefix, RSS feed, Hugging Face dataset, database query, local directory, MCP or A2A resource | Ogun; browser-session origins through Zeus |
| Artifact | An immutable materialization: `paper.pdf`, `dataset.parquet`, `page.html`, `model.safetensors` | Format in `zeus-core`; Ogun's own store |
| Asset | A logical object with many versions or materializations | Ogun |
| Operation | Something done to one or more artifacts: download, crawl, extract, decompress, convert, OCR, parse, filter, join, embed, classify, rename, move, upload, query, LLM-extract | Ogun (downloads run on the embedded engine) |
| Run | One durable execution of a workflow | Ogun |
| Workflow | A versioned declarative specification | Ogun |
| Agent | A human or machine actor that plans or performs operations | Ogun |
| Policy | Permissions, budgets, rate limits, approval requirements | Ogun; session grants stay Zeus policy |
| Environment | The isolated place a run executes: base image, fixtures, workspace, tool policy | Ogun |
| Evaluator | Something that scores a run's outputs | Ogun |

An artifact knows its id, content hash, size, MIME type, retrieval time, origin and source, licence, parent artifacts, producing operation, workflow and run, verification state and metadata. Zeus spec 04 already records most of these per file in the provenance manifest. Making them a first-class artifact with a stable id extends that ledger rather than redesigning it. The format is a `zeus-core` library (Zeus spec 13, implemented as `zeus-artifact`) that both products use.

An asset groups versions, for example *WHO cholera surveillance dataset*: 2026-09-01, 2026-09-08, 2026-09-15. Dagster's asset-oriented model is the reference to study.

With Environment and Evaluator in the model, a run becomes:

```text
Run = Workflow + Environment + Agent + Policy + Inputs + Evaluator
```

A benchmark is then just a controlled collection of runs (section 12). Once these objects exist, much of the functionality stops being special cases.

## 4. The ADE analogy

Agentic development environments organize work around runs and tasks rather than editor buffers: parallel orchestration, isolated execution, approval gates, persistent state, instrumentation. Each of those concepts translates directly:

| Coding ADE | Ogun |
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

Ogun needs a real durable workflow engine, but a lightweight one. The engine's scheduler is a weighted-fair download queue; Ogun adds a durable run and node state machine above it:

`pending`, `ready`, `running`, `waiting`, `waiting_for_user`, `waiting_for_session`, `retry_scheduled`, `succeeded`, `failed`, `cancelled`, `skipped`.

`waiting_for_user` and `waiting_for_session` map onto the engine's job states `awaiting_approval` and `awaiting_session` (the latter only for jobs on the Zeus session bridge). A run holds node instances, artifact edges, checkpoints, retry state, an event log and provenance records. A run must survive a daemon crash, a reboot or a closed browser, because agentic work is long.

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
ogun backfill climate-data --from 2020-01-01 --to 2026-10-05
```

## 6. Operators

Ogun orchestrates existing tools. It does not reimplement them.

| Kind | Operators |
|---|---|
| Core (Rust) | `acquire` (embedded `zeus-core` engine), `zeus.session_acquire` (browser-session sources through the Zeus bridge), `crawl`, `file.copy`, `file.move`, `archive.extract`, `hash`, `dedupe`, `manifest`, `partition`, `filter`, `map`, `join_metadata` |
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
from ogun import workflow, acquire

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
    uses: acquire
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

Provenance may become more strategically valuable than acquisition itself. Every record of a dataset built by Ogun can carry:

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

Physical blobs are stored by hash (`objects/blake3/ab/cd/abcdef...`, Zeus spec 13), and logical projects reference them. Downloading the same 20 GB model twice does not duplicate storage, and a transformed dataset shares its unchanged parts. The path runs from file-level CAS to chunk-level CAS, then content-defined chunking, then a remote artifact cache (Zeus research doc 06 covers the mathematics). The store format is a `zeus-core` library; Ogun runs its own store instance (ADR 0002).

## 10. Agents

**Agent runs are first-class objects.** Each records the agent, task, plan, permissions, workspace, tools, inputs, outputs, model, tokens, cost, wall time, network bytes, artifacts created, decisions and approval events. A developer can see:

```text
Claude Code          requested workflow
Ogun planner   generated workflow v17
Extractor agent      7,284 documents, $1.38 model cost
Quality agent        rejected 213 records
Ogun engine          43.7 GB acquired
```

**MCP** exposes a small, hierarchical surface (workflow create, validate, plan, run, pause, resume, backfill; source inspect and discover; artifact query, lineage, materialize; dataset describe and validate; schedule create; run status, logs, retry; session-backed acquisition through the Zeus bridge), plus resources. It does not expose hundreds of tiny tools, because tool schemas cost context (Zeus research doc 10).

**A2A** finally has a reason to exist: Ogun is a peer in agent pipelines. A research agent asks it to acquire SEC filings; it hands artifacts to an extraction agent; that agent hands structured records to an analysis agent. Or Codex writes an extractor, Ogun runs it over 100,000 files, a QA agent examines the failures, Codex fixes the extractor, and Ogun retries only the failed partitions. A2A remains an interface to the runtime, not the runtime.

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

The approve button is one of Ogun's own human trust surfaces. It follows the rules of Zeus ADR 0007: human-only, bound to a request hash, never reachable from MCP or A2A. If an approval concerns a browser session, it is a Zeus approval, and Studio links to the Zeus surface instead of deciding it.

## 12. Benchmark profile

Ogun can serve as a harness for data and terminal agents: Spider 2.0-DBT (68 DuckDB and dbt tasks), ADE-bench (dbt Labs; modelled on Terminal-Bench; dbt repositories with a DuckDB or Snowflake database; a sandbox per task) and Terminal-Bench 2.0 (distributed in the Harbor task format and run with the Harbor harness). These benchmarks sit almost exactly in the Ogun domain: repositories, SQL, dbt, datasets, terminals, transformations, generated artifacts and validation.

**Product mode optimizes for completing the task; benchmark mode optimizes for measuring the agent.** Benchmark mode must therefore switch off helpful behaviour that contaminates evaluation: automatic retries and repairs, schema inference, help from other agents, cached artifacts and recovered downloads. Otherwise the measurement is "Ogun plus model", not the configuration under test. ADR 0005 records the rules:

- Isolation: immutable environment (base image, fixture, task repository, agent binary, tool policy) in a container, a microVM later. Network denied by default. `/workspace` read-write, `/benchmark` read-only, the rest hidden. Model credentials only. No access to the user's Ogun or Zeus artifact stores, to `zeusd` or to browser sessions.
- Cache modes `disabled`, `benchmark_provided`, `cold`, `warm`. Fresh memory, session and workspace unless a longitudinal study asks otherwise.
- The trajectory is a replayable event stream (`task.started`, `model.request`, `model.response`, `tool.shell`, `tool.result`, `file.read`, `file.patch`, and so on), alongside the initial repository hash, environment digest, task version, agent and model versions, prompt and tool-definition hashes, database state before and after, evaluator output, tokens, latency and cost. `ogun bench replay <run>` and the Studio timeline render it.

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

Ogun does not win by "running Spider". Harbor, ADE-bench's own harness and Spider 2.0's agent framework already do that, and multi-benchmark harnesses for data agents are appearing. It wins if researchers can run benchmark families × agents × models × harness configurations × seeds in controlled environments, and get reproducibility, trajectories, lineage, cost accounting, failure taxonomies, harness ablations and cross-benchmark analysis. Spider2-DBT and ADE-bench are also the best **integration tests** for the architecture itself. If the runtime can execute their tasks, capture trajectories, reset environments and run evaluations, the core abstractions hold.

## 13. What Ogun will not build

Its own dataframe library, SQL engine, browser, OCR engine, vector database or foundation model; a full Airflow or dbt replacement; autonomous captcha or anti-bot circumvention; a generic RPA platform. Ogun integrates these (ADR 0004). Its coherent responsibility is **materializing trustworthy data artifacts from heterogeneous sources, and orchestrating everything required around that materialization.**

## 14. Position

The comparison set is no longer IDM, Gopeed, aria2 and JDownloader. It is an intersection of Gopeed and aria2, Firecrawl and Crawl4AI, dlt and Airbyte, Prefect, Dagster and Kestra, OpenRefine, rclone, MCP and A2A runtimes, and coding-ADE interaction patterns. Ogun should not try to equal any of them. Its value is the intersection: **the local-first agentic runtime that turns authenticated, messy web and filesystem sources into verified, reproducible data assets.**

Orchestration is crowded and consolidating. Prefect announced its acquisition of Dagster Labs on 2026-07-13, and Dagster continues as a separate open-source product. So Ogun is not "a better Airflow". It contains enough durable orchestration to run acquisition workflows by itself, and it plugs into the orchestrators: as a Prefect task, a Dagster resource, an Airflow operator, a Kestra plugin, a Temporal activity, a GitHub Action, an MCP server and an A2A agent.

Candidate taglines, undecided: "From browser session to reproducible dataset". "A durable data plane for agents". "The acquisition and artifact runtime for AI agents" (the last describes Zeus plus Ogun).

## 15. Scope discipline

The original research warned that building the GUI, workflows, A2A, a file manager and a downloader at once would mean catastrophic scope creep. That warning still holds, so the order is deliberate: `zeus-core` engine, then Ogun Workflows, then Ogun Studio. The Zeus consumer app proceeds on its own track.

1. Ogun's runtime starts when the `zeus-core` engine survives a crash and resumes (kill the process mid-download, restart, matching hash). The Zeus smoke test showed this on 2026-10-18, so runtime work has started (ADR 0006). It does not wait for the Zeus extension or the consumer app.
2. Before much implementation, design the four contracts that decide whether Ogun becomes this platform or gets trapped as a sophisticated downloader: the **workflow IR**, the **artifact and asset model**, the **run state machine** and the **operator ABI**.

## 16. Name

"Ogun" (chosen 2026-10-06; the earlier codename was Hephaestus) is far less crowded than the alternatives in software. A GitHub search finds no notable software project of that name. On crates.io, `ogun` is taken by a small game-theory and procedural-layout crate, while `ogund` and `ogun-core` are free. The binaries are `ogun` (CLI) and `ogund` (daemon); the workspace crates are `ogun-*` and unpublished. Ogun is also the Yoruba orisha of iron and technology, still venerated today, and the name of a Nigerian state. The trademark clearance that Zeus ADR 0006 requires before launch should cover the cultural side too.
