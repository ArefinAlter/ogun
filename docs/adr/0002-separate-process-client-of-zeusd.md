# ADR 0002: Hephaestus is a separate process and a client of zeusd

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-06 |
| Zeus counterpart | `zeus/docs/adr/0011-hephaestus-split.md` (Proposed) |

## Context

Zeus is the session-aware acquisition and file layer: daemon `zeusd`, engine library `zeus-core`, MV3 extension. Its trust model rests on a few hard rules. `zeusd` is the only process that holds session material. It has one listener (Zeus ADR 0010). Approvals are human-only (Zeus ADR 0007). `zeus_api::policy::METHODS` is the single table of who may call what.

Hephaestus adds workflows, schedules, operators (several of which run arbitrary code: Python, shell, dbt), model calls, assets, lineage, a GUI and a benchmark harness ([`../VISION.md`](../VISION.md)). If that functionality landed inside `zeusd`, the process that holds cookies would also run user and LLM-written code. It would grow without bound, and the auditable core that justifies the extension's permissions would be lost.

## Decision

1. **Ownership.** Zeus owns acquisition, browser sessions, grants, approvals and the human trust surfaces that decide them, the **artifact identity space and store** (content hashes, `art_` ids, CAS layout, acquisition provenance), and acquisition plans (Zeus spec 08, rescoped by Zeus ADR 0011). Hephaestus owns workflows, runs and node instances, schedules and triggers, partitions and backfills, operators, assets and datasets, transform lineage, agent-run records, Studio and the benchmark profile.
2. **Separate process.** Hephaestus runs as its own daemon, `hephd`, with its own durable state. It is an ordinary client of `zeusd` over the Zeus daemon API, authenticated with a non-human credential. It may link `zeus-types` and the `zeus-api` client. It never links `zeus-engine`, `zeus-session` or `zeus-journal`, and never opens Zeus state files.
3. **No session material.** Authenticated acquisition is always a Zeus job or a Zeus acquisition-plan run, referenced by id. A node whose Zeus job reaches `awaiting_session` or `awaiting_approval` moves to `waiting_for_session` or `waiting_for_user`. Hephaestus never receives cookies, tokens or URL query strings in clear text.
4. **No approval decisions.** No Hephaestus surface that agents can reach (MCP, A2A, API, scheduler) may decide an approval, create or extend a grant, or return session material. This is Zeus constraint 1, applied unchanged. Approvals that Hephaestus needs (enabling a code-executing operator, spending over budget, publishing outside the workspace) are decided by a human on a trust surface. Where they are raised and rendered is open question Q1.
5. **One artifact identity space.** Artifacts produced by Hephaestus operators are registered in the Zeus artifact store and receive `art_` ids there, so deduplication, provenance and lineage span acquisition and transformation. The write path (ingest through `zeusd`, or a multi-writer filesystem CAS library in `zeus-core`) is decided in Zeus spec 13 (planned).
6. **Code runs out of process.** Operators that execute code run in sandboxed child processes of `hephd` (or of a worker), never inside `zeusd`, and never with access to Zeus's state directory or credential files.
7. **Shared conventions.** Hephaestus adopts Zeus spec 00 by reference: JSON on the wire, instants, durations, the error envelope, redaction, prefixed ULIDs. It registers its own id prefixes in Zeus spec 00's prefix table, so ids never collide. Zeus's `run_` remains the Zeus acquisition-plan run.

## Consequences

- Zeus can ship and be used without Hephaestus. Hephaestus requires a running `zeusd`, so installing Hephaestus installs Zeus.
- The full stack has two daemons. That costs one more process and one more state directory, and buys a clean security boundary.
- The Zeus daemon API becomes a public contract for a second first-party consumer. Freezing it as v1 (Zeus roadmap phase 2) matters earlier.
- The Hephaestus run state machine is defined in Hephaestus specs, and it maps Zeus job states instead of copying them.

## Alternatives considered

- **Workflows inside `zeusd`.** Rejected: it puts arbitrary code execution and model calls in the cookie-holding process and makes the core unauditable.
- **Hephaestus embeds `zeus-core` and runs its own engine.** Rejected: that means two processes holding session material, two journals and a split artifact identity.
- **Hephaestus independent, with Zeus as an optional source plugin.** Rejected: it duplicates the artifact store and demotes the one differentiated capability (authenticated acquisition) to a plugin.

## Open questions (decide before accepting)

| # | Question | Recommendation |
|---|---|---|
| Q1 | Where are Hephaestus approvals raised and decided? | One inbox: Zeus approvals gain a generic kind that service clients may raise, with facts Zeus renders verbatim. The same channel evidence and request-hash rules apply. The human has one queue. |
| Q2 | One GUI or two? | One: Hephaestus Studio is a human-kind client of both daemons and replaces the Zeus Tauri GUI of Zeus ADR 0009 (which needs a superseding Zeus ADR). |
| Q3 | Who owns the LLM provider ladder (`zeus-llm`, Zeus ADR 0008, Zeus spec 09)? | Hephaestus, because LLM operators are its main consumer. Zeus keeps only rung 1 (the host agent over MCP) and needs no model access of its own. |
| Q4 | Who owns A2A (`zeus-a2a`, Zeus spec 10)? | Hephaestus, because the peer that agents talk to is a workflow-level agent. Zeus exposes MCP only. |
| Q5 | Which client kind does `hephd` hold in Zeus? | A new non-human kind `service`, with the agent ceiling plus `artifacts.write`, so audit records can tell a scheduled Hephaestus run from an MCP agent. Alternative: reuse `agent`. |
