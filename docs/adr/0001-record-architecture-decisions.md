# ADR 0001: Record architecture decisions

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-06 |

## Context

Hephaestus is designed across conversations, devices and coding agents, next to three Zeus repositories that already use ADRs (`zeus/docs/adr`). Decisions made in conversation get lost, and the next contributor re-argues them.

## Decision

Use the same scheme as Zeus ADR 0001. One Markdown file per decision in `docs/adr/`, numbered `NNNN-short-title.md`, in Michael Nygard's style. Each record has: Status (`Proposed`, `Accepted`, `Superseded by NNNN`), Date, Context, Decision, Consequences, Alternatives considered. Once accepted, a record is immutable; to change it, add a superseding record.

An ADR is required when a change affects a contract in `docs/spec`, a trust boundary, a licence, a persistent format, the crate dependency graph, or **the boundary with Zeus**. A decision that changes a Zeus contract also needs a Zeus ADR. The two records reference each other.

## Consequences

Newcomers can read the records in order. A `Proposed` record states a recommendation and its open questions. Specs may not depend on a record until the maintainer accepts it.

## Index

| ADR | Title | Status |
|---|---|---|
| 0001 | Record architecture decisions | Accepted |
| 0002 | Hephaestus is a separate process and a client of zeusd | Proposed |
| 0003 | The workflow IR is data, not code | Proposed |
| 0004 | Integrate, do not replace | Proposed |
| 0005 | Benchmark runs are a run profile | Proposed |
