# ADR 0002: Ogun is a separate product built on zeus-core

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-06 |
| Decided by | Maintainer, 2026-10-06 (product split, name, two GUIs, LLM ladder and A2A in both products). Q5 was delegated; the answer here follows from the other answers. |
| Zeus counterpart | `zeus/docs/adr/0011-ogun-built-on-zeus-core.md` |

## Context

Zeus has two layers. `zeus-core` is the researched download mechanism: segmented resumable transfer, journal and crash-safe commit, verification, adaptive control. The `zeus` app is the consumer product: an agentic download and file manager, as the original research describes, with the MV3 extension, the native host, approvals, MCP, workflows, an LLM provider ladder, A2A and a GUI.

The same download mechanism is valuable to a different audience: AI, LLM and data engineers who mine data, build datasets, run data-engineering workflows and benchmark agents ([`../VISION.md`](../VISION.md)). Their needs differ from the consumer product: DAGs, partitions, backfills, operators that run code, lineage, environments, evaluators, and a workbench GUI. Folding those into the consumer app would bloat it and put arbitrary code execution next to browser sessions.

## Decision

1. **Two products on one engine.** `zeus-core` is the shared engine library. The `zeus` app remains the consumer agentic download and file manager on its original roadmap. **Ogun** is a separate developer platform for data mining, data engineering and benchmarking, built directly on `zeus-core`.
2. **What Ogun embeds.** Ogun links the `zeus-core` crates and runs its own `Engine` in its own daemon, `ogund`. It builds that engine with a session provider and a proxy-fetch port that always answer "no session". Ogun's engine therefore never holds browser cookies. It handles public sources and sources authenticated by Ogun's own credentials (API tokens, S3 keys and similar), which follow Zeus constraint 3: zeroized, never logged, never serialized.
3. **Browser sessions stay in Zeus.** Only `zeusd` talks to the extension and holds session material (Zeus ADR 0004, Zeus ARCHITECTURE section 1). When an Ogun workflow needs a source behind the user's browser session, Ogun asks `zeusd` to run that acquisition as a Zeus job, through the **optional session bridge** (decision 7). `zeusd` downloads into a write root registered for the Ogun workspace, and Ogun imports the file together with its Zeus provenance record. Without Zeus installed, Ogun works fully except for browser-session sources.
4. **Two GUIs.** The Zeus GUI (Zeus ADR 0009) is the consumer download and file manager. **Ogun Studio** is the developer workbench: runs, workflows, artifacts, lineage, datasets, agents, schedules, benchmarks.
5. **Both own an LLM provider ladder and A2A.** Each product has its own ladder configuration, provider credentials, signed policy manifest, spend ledger and approval rules. Each has its own A2A agent card and endpoint: Zeus is an acquisition agent, Ogun a data-workflow agent. Both endpoints are bridge processes, off by default (the Zeus ADR 0010 pattern). Ogun's model use is optional: a workflow with no LLM operator runs with no model access.
6. **Both own their approvals.** Ogun decides approvals in its own domain (enabling a code-executing operator, spending over budget, publishing outside the workspace) on its own human trust surfaces: the Ogun CLI on a tty, and Ogun Studio. It applies the rules of Zeus ADR 0007 unchanged: human-only, bound to a request hash, channel evidence, never decidable over MCP, A2A or any agent-reachable path. Session release is always a Zeus approval, decided on a Zeus trust surface.
7. **Q5: Ogun is a `service` client of `zeusd`.** Zeus adds a fourth client kind, `service`, for first-party programs that use `zeusd` unattended. Its ceiling is the agent ceiling plus registration of its own write roots. It is never human, never decides approvals and never receives session material. Ogun's bridge credential has this kind. Reasons: scheduled Ogun runs need session-backed jobs without an agent in the loop, Zeus audit records must attribute every session use to Ogun rather than to "an agent", and service clients need their own rate limits. A human links Ogun to Zeus once with the Zeus CLI, which provisions the credential. Unattended session use then stays within the grants that a human created on a Zeus trust surface (Zeus spec 01 section 5).
8. **Shared code lives in `zeus-core`; product state does not.** Ogun depends on `zeus-core` crates, plus `zeus-api` for the session bridge (the daemon API contract), and nothing else from the `zeus` app repository. When Ogun first needs code that today lives in the app (`zeus-llm`, `zeus-a2a`, the pure plan-IR parts of `zeus-workflow`), that code moves to `zeus-core` through a Zeus ADR. Configuration, credentials, ledgers and state stay per product.
9. **Artifacts.** The artifact and content-addressed store format is a `zeus-core` library (planned Zeus spec 13). Each product runs its own store instance. The content hash is the identity across products, so a file acquired by Zeus and imported into Ogun keeps its hash and provenance.
10. **Shared conventions.** Ogun adopts Zeus spec 00 by reference (JSON, instants, durations, error envelope, redaction, prefixed ULIDs) and registers its id prefixes in Zeus spec 00's table so ids never collide.

## Consequences

- Ogun can start its runtime once the `zeus-core` engine works. It does not wait for the Zeus extension or the consumer app.
- `zeus-core` becomes a library with two first-party consumers, so its public API needs the same care as the daemon API.
- Some work is duplicated by design (two GUIs, two approval inboxes, two A2A endpoints), and two ladder configurations exist. Shared crates keep the duplicated *code* small.
- A user who installs both has two daemons. Browser-session acquisition for Ogun requires `zeusd` and a one-time human link.

## Alternatives considered

- **Ogun as a client of `zeusd` only, with no embedded engine:** rejected by the maintainer. Ogun is a standalone developer tool on the engine, not an add-on to the consumer app.
- **Ogun with its own extension or native host for sessions:** rejected. It would mean two cookie-holding processes and two consent surfaces in the browser.
- **One shared GUI or one approval inbox:** rejected by the maintainer. The audiences and workflows differ.
- **Reusing the `agent` kind for the bridge:** rejected. Audit would conflate Ogun's scheduled runs with interactive agents, and there would be no separate limits.
