# Ogun

Ogun is a local-first developer platform for data mining, data engineering and agent benchmarking, built on the Zeus download engine (`zeus-core`). It is for AI, LLM and data engineers who need to turn messy web and filesystem sources into verified, reproducible datasets on a schedule, and to let agents plan and operate that work.

> **Status: pre-alpha.** Ogun Studio on Windows owns its engine (0.0.1b). `ogund` runs workflows end to end: durable runs and partitions, backfills that survive `kill -9`, schedules, streams as windowed partitions, approvals, artifacts and lineage, with `ogun-server` for a team hub (see [`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md) for what runs and the known gaps). The specs O-00 to O-17 are drafts or proposals (`docs/spec/`).

## Try the pre-alpha

- **Download:** [release v0.0.1b](https://github.com/ArefinAlter/ogun/releases/tag/v0.0.1b). **Ogun Studio is a standalone desktop app**: one installer, its own engine (`ogund`, started and stopped by the app on a private pipe), no daemon to start, no account and no network needed. Windows x64 (installer, portable zip) and Linux x64 (AppImage, .deb), plus the `ogun`, `ogund` and `ogun-server` binaries as archives. Unsigned. What was checked on Windows, how, and what is only partly verified: [`docs/ACCEPTANCE-0.0.1b.md`](docs/ACCEPTANCE-0.0.1b.md).
- **Hosted:** `https://ogun.103.191.50.32.sslip.io`, Ogun Studio-web on the real daemon with sample data; the sign-in page shows a shared sample account, and everything is reset every night. It still runs the 0.0.1a backend: desktop sign-in to it (browser sign-in from Studio desktop) works only after the owner applies [`deploy/pre-alpha/native-signin`](deploy/pre-alpha/native-signin/README.md). How the hosted hub is built: [`deploy/pre-alpha`](deploy/pre-alpha/README.md).
- **Windows sandbox:** code operators run in an AppContainer with no network and no access to your files beyond their inputs; the limits are stated in [`docs/windows-sandbox.md`](docs/windows-sandbox.md).

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
| `zeus-core` | Shared engine library: types, control, filesystem, verification, journal, session protocol, HTTP, engine, artifact store (`zeus-artifact`) |
| `zeus` | Zeus consumer app (`zeusd`, CLI and TUI, MCP, native host) and the normative Zeus specs |
| `zeus-extension` | Zeus MV3 browser extension |
| [`ogun-site`](https://github.com/ArefinAlter/ogun-site), `zeus-site` | The product websites: [ogun-site.vercel.app](https://ogun-site.vercel.app) and [zeus-site-bay.vercel.app](https://zeus-site-bay.vercel.app) |

## Editors, agents and data platforms

None of these can approve anything; approvals happen on the Ogun CLI or in Studio ([O-14](docs/spec/O-14-extension-and-integration-surfaces.md)).

| Path | What |
|---|---|
| `crates/ogun-lsp` | `ogun lsp` / `ogun-lsp`: language server for `*.workflow.json` (validator diagnostics, hover, completion, go-to-definition, symbols, workflow hash) |
| `extensions/vscode` | VS Code extension: the language server when found, `ogun validate --json` otherwise |
| `integrations/mcp` | `ogun mcp` configurations for Claude Code, Claude Desktop, Cursor, VS Code, Windsurf, Zed and Codex CLI |
| `integrations/python/ogun-client` | Python client of the `ogund` local API (O-15) |
| `integrations/airflow`, `dagster`, `prefect`, `kestra` | Orchestrator integrations that start and watch Ogun runs with an `agent` credential |

## Read next

1. [`docs/VISION.md`](docs/VISION.md): the product, the object model, the benchmark profile.
2. [`docs/adr/`](docs/adr/): the decisions.
3. [`ROADMAP.md`](ROADMAP.md): contracts first, then the runtime.

## Building

The workspace path-depends on `zeus-core` as a sibling checkout:

```sh
git clone https://github.com/ArefinAlter/zeus-core ../zeus-core
cargo test --workspace
cargo run -p ogun-cli -- validate examples/climate-papers.workflow.json
```

Rust 1.90 or newer.

## Licence

Apache-2.0, like Zeus (Zeus ADR 0006). Ogun is a product of Verne Ltd. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
