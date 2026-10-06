# CLAUDE.md: orientation for coding agents

Hephaestus is a local-first agentic data environment: workflows, operators, assets, lineage, Studio and a benchmark profile, built on Zeus. Apache-2.0, open core. Pre-alpha, **contracts phase: there is no code yet.** "Hephaestus" is a codename.

## Four repositories, side by side

| Path | Role |
|---|---|
| `hephaestus/` (this repo) | Workflow runtime, operators, Studio, benchmark profile. Docs only for now. |
| `../zeus/` | Zeus app (`zeusd`, CLI/TUI, MCP, native host) and Zeus specs. While `../zeus/HANDOFF.md` exists, it is the cross-device build state; read it. |
| `../zeus-core/` | Zeus engine crates. Never edit from here. |
| `../zeus-extension/` | Zeus MV3 extension. Never edit from here. |

## Repo map

| Path | What |
|---|---|
| `docs/VISION.md` | Product direction and object model. Not normative. |
| `docs/adr/` | Decisions. `Proposed` records are recommendations; do not build on them until accepted. |
| `docs/spec/` | Normative specs H-00 to H-06 (planned, see `ROADMAP.md`). Prose wins over schema. |
| `docs/research/` | Research corpus HR1 to HR4 (planned). Unverified unless front matter says `verified`. |

## Constraints (do not violate)

1. **Hephaestus is a client of zeusd** (ADR 0002). Never link `zeus-engine`, `zeus-session` or `zeus-journal`; never open Zeus state or credential files. Only `zeus-types` and the `zeus-api` client.
2. **No session material, no approval decisions.** No Hephaestus path reachable by agents (MCP, A2A, API, scheduler) may decide an approval, create or extend a grant, or receive cookies, tokens or URL query strings. Zeus constraints 1 to 3 apply unchanged (`../zeus/CLAUDE.md`).
3. **The workflow IR is data** (ADR 0003): JSON, no floats, JCS + BLAKE3 hash, closed template and predicate sets. Never a path from prompt to arbitrary shell. Code-executing operators are referenced by content hash, run sandboxed out of process, and need a human approval bound to the workflow hash.
4. **Models are optional.** A workflow without `llm.*` operators must run with no model access.
5. **Benchmark isolation** (ADR 0005): benchmark runs never touch the user's artifact store, `zeusd` or browser sessions; network is denied by default.
6. Shared conventions come from Zeus spec 00 (JSON `snake_case`, RFC 3339 UTC with milliseconds, `_ms` durations, prefixed ULIDs, error envelope, redaction). Register new id prefixes in Zeus 00's table.
7. When code starts: Rust edition 2024, MSRV and lint policy as in Zeus, `#![forbid(unsafe_code)]` outside isolated platform modules, thiserror in libraries, anyhow only in binaries, tracing, no `todo!()`, hermetic tests, no telemetry.
8. Do not run mutating git commands unless the user asks; the maintainer commits.
9. A decision that changes a Zeus contract needs a Zeus ADR too (counterpart of ADR 0002: `../zeus/docs/adr/0011-hephaestus-split.md`).
