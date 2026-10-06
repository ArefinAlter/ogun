# CLAUDE.md: orientation for coding agents

Ogun is a local-first developer platform for data mining, data engineering and agent benchmarking, built on the Zeus engine library `zeus-core`. Apache-2.0, open core. Pre-alpha, **contracts phase: there is no code yet.**

## Four repositories, side by side

| Path | Role |
|---|---|
| `ogun/` (this repo) | Workflows, operators, lineage, Ogun Studio, benchmark profile. Docs only for now. |
| `../zeus-core/` | Shared engine library that Ogun embeds. Never edit it from here without being asked; changes need a Zeus ADR. |
| `../zeus/` | Zeus consumer app and the Zeus specs (00 to 04 written). While `../zeus/HANDOFF.md` exists it is the cross-device build state; read it. |
| `../zeus-extension/` | Zeus MV3 extension. Not used by Ogun directly. |

## Repo map

| Path | What |
|---|---|
| `docs/VISION.md` | Product direction and object model. Not normative. |
| `docs/adr/` | Decisions. ADR 0002 (the product boundary) is accepted; `Proposed` records are recommendations, so do not build on them until they are accepted. |
| `docs/spec/` | Normative specs O-00 to O-06 (planned, see `ROADMAP.md`). Prose wins over schema. |
| `docs/research/` | Research corpus OR1 to OR4 (planned). Unverified unless the front matter says `verified`. |

## Constraints (do not violate)

1. **Built on `zeus-core`** (ADR 0002). Depend only on `zeus-core` crates and on `zeus-api` (for the session bridge). Nothing else from the `zeus` app repository. Shared code moves to `zeus-core` through a Zeus ADR, never by path-depending on app crates.
2. **No browser session material, ever.** Ogun's embedded engine uses a session provider and proxy-fetch port that always answer "no session". Browser-session sources go through `zeusd` as Zeus jobs on the session bridge (`service` client kind). Never read browser cookie stores or profile files (Zeus ADR 0004).
3. **Approvals are human-only.** Ogun's approvals are decided only on Ogun's human trust surfaces (CLI on a tty, Studio), with the rules of Zeus ADR 0007: request-hash binding and channel evidence. No path that agents can reach (MCP, A2A, API, scheduler) may decide an approval. Session-release approvals are always Zeus's.
4. **Secrets** (Ogun's provider keys, S3 and API tokens, URL query strings) never reach logs, events, lineage, errors, fixtures or LLM prompts. Follow Zeus constraint 3: `redact_url`, zeroizing types, no `Serialize` on secret types, `ZEUS-CANARY-` leak markers.
5. **The workflow IR is data** (ADR 0003): JSON, no floats, JCS + BLAKE3 hash, closed template and predicate sets. There is never a path from prompt to arbitrary shell. Code-executing operators are referenced by content hash, run sandboxed out of process, and need a human approval bound to the workflow hash.
6. **Models are optional.** A workflow without `llm.*` operators must run with no model access. Ogun has its own LLM provider ladder configuration and spend ledger.
7. **Benchmark isolation** (ADR 0005): benchmark runs never touch the user's artifact stores, `zeusd` or browser sessions; network is denied by default.
8. Shared conventions come from Zeus spec 00 (JSON `snake_case`, RFC 3339 UTC with milliseconds, `_ms` durations, prefixed ULIDs, error envelope, redaction). Register new id prefixes in Zeus 00's table.
9. When code starts: Rust edition 2024, MSRV and lint policy as in Zeus, `#![forbid(unsafe_code)]` outside isolated platform modules, thiserror in libraries, anyhow only in binaries, tracing, no `todo!()`, hermetic tests (loopback only), no telemetry, one local listener per daemon with HTTP surfaces as bridge processes (Zeus ADR 0010 pattern).
10. Do not run mutating git commands unless the user asks; the maintainer commits.
