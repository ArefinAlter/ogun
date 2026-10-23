# CLAUDE.md: orientation for coding agents

Ogun is a local-first developer platform for data mining, data engineering and agent benchmarking, built on the Zeus engine library `zeus-core`. Apache-2.0, open core. A Verne Ltd. product. Pre-alpha: the contracts (O-00 to O-03) are in draft and the first runtime crates are being built.

## Repositories, side by side

| Path | Role |
|---|---|
| `ogun/` (this repo) | Workflows, operators, lineage, Ogun Studio, benchmark profile. Specs in `docs/spec/`, Rust workspace in `crates/`. |
| `../zeus-core/` | Shared engine library that Ogun embeds. Never edit it from here without being asked; changes need a Zeus ADR. |
| `../zeus/` | Zeus app (`zeusd`, CLI and TUI, MCP, A2A bridge) and the Zeus specs 00 to 14 (Draft 1). Its build state is `docs/PROJECT_HANDOFF.md` and `docs/IMPLEMENTATION_STATUS.md`; read them before relying on a Zeus behaviour. |
| `../zeus-extension/` | Zeus MV3 extension. Not used by Ogun directly. |
| `../ogun-site/`, `../zeus-site/` | The two product websites (static, Node). Company name in all public copy: Verne Ltd. |

## Repo map

| Path | What |
|---|---|
| `docs/VISION.md` | Product direction and object model. Not normative. |
| `docs/adr/` | Decisions. ADR 0002 (the product boundary) is accepted. ADRs 0003 to 0006 are proposed. The maintainer asked for implementation to start on 2026-10-23, so `crates/` follows ADR 0003 and 0006 ahead of their acceptance; if either changes, the code changes with it. Do not build on 0004 or 0005 until they are accepted. |
| `docs/spec/` | Normative specs O-00 to O-08. O-00 to O-03 are Draft 1; the rest are planned (see `ROADMAP.md`). Prose wins over schema. |
| `docs/research/` | Research corpus OR1 to OR4 (planned). Unverified unless the front matter says `verified`. |
| `crates/` | Rust workspace: `ogun-types` (ids, errors), `ogun-ir` (workflow IR, validator, hash, plan diff), `ogun-run` (run and node state machine), `ogun-sched` (cron, partitions, backfill plans, budget reservations), `ogun-cli` (the `ogun` binary). |

## Constraints (do not violate)

1. **Built on `zeus-core`** (ADR 0002). Depend only on `zeus-core` crates, through the sibling path `../zeus-core`. Nothing from the `zeus` app repository, including `zeus-api`, which now pulls in app crates (proposed ADR 0006). The session bridge speaks Zeus spec 05 through Ogun's own client. Shared code moves to `zeus-core` through a Zeus ADR, never by path-depending on app crates.
2. **No browser session material, ever.** Ogun's embedded engine uses a session provider and proxy-fetch port that always answer "no session". Browser-session sources go through `zeusd` as Zeus jobs on the session bridge (`service` client kind). Never read browser cookie stores or profile files (Zeus ADR 0004).
3. **Approvals are human-only.** Ogun's approvals are decided only on Ogun's human trust surfaces (CLI on a tty, Studio), with the rules of Zeus ADR 0007: request-hash binding and channel evidence. No path that agents can reach (MCP, A2A, API, scheduler) may decide an approval. Session-release approvals are always Zeus's.
4. **Secrets** (Ogun's provider keys, S3 and API tokens, URL query strings) never reach logs, events, lineage, errors, fixtures or LLM prompts. Follow Zeus constraint 3: `redact_url`, zeroizing types, no `Serialize` on secret types, `ZEUS-CANARY-` leak markers.
5. **The workflow IR is data** (ADR 0003): JSON, no floats, JCS + BLAKE3 hash, closed template and predicate sets. There is never a path from prompt to arbitrary shell. Code-executing operators are referenced by content hash, run sandboxed out of process, and need a human approval bound to the workflow hash.
6. **Models are optional.** A workflow without `llm.*` operators must run with no model access. Ogun has its own LLM provider ladder configuration and spend ledger.
7. **Benchmark isolation** (ADR 0005): benchmark runs never touch the user's artifact stores, `zeusd` or browser sessions; network is denied by default.
8. Shared conventions come from Zeus spec 00 (JSON `snake_case`, RFC 3339 UTC with milliseconds, `_ms` durations, prefixed ULIDs, error envelope, redaction). New id prefixes go in O-00's table first and then into Zeus 00 section 5.1 through a Zeus pull request; never reuse a Zeus prefix (`run_` is Zeus's workflow run).
9. Rust: edition 2024, MSRV 1.90 (as `zeus-core`), `#![forbid(unsafe_code)]` outside isolated platform modules, thiserror in libraries, anyhow only in binaries, tracing for logs, no `todo!()` or `unimplemented!()` (return a typed error), doc comments on public items, dependencies declared once in the root `[workspace.dependencies]`, hermetic tests (loopback only), no telemetry, one local listener per daemon with HTTP surfaces as bridge processes (Zeus ADR 0010 pattern). Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` before committing.
10. Do not run mutating git commands unless the user asks; the maintainer commits.
