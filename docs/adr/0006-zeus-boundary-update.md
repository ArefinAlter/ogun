# ADR 0006: The Zeus boundary, updated for the Zeus implementation

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-23 |
| Decided by | Maintainer, 2026-10-23 ("accept for all") |
| Amends | [ADR 0002](0002-separate-product-on-zeus-core.md), decisions 8 and 9, and its first consequence |

## Context

ADR 0002 was written on 2026-10-06, when Zeus had specs 00 to 04 and no code. By 2026-10-21 the Zeus repositories had drafted specs 00 to 14 and implemented most of phase 1 (`zeus/docs/IMPLEMENTATION_STATUS.md`). Three statements in ADR 0002 no longer hold:

1. **Decision 8** lets Ogun depend on `zeus-api` for the session bridge. `zeus-api` now depends on the app crates `zeus-workflow`, `zeus-llm` and `zeus-cloud` (`zeus/crates/zeus-api/Cargo.toml`), so depending on it would pull app code into Ogun, which the same decision forbids.
2. **Decision 9** calls the artifact store format "planned Zeus spec 13". Spec 13 is written (Draft 1) and implemented as the `zeus-core` crate `zeus-artifact`. Objects are named by BLAKE3: `objects/blake3/<2 hex>/<2 hex>/<64 hex>`.
3. **Consequence 1** gates Ogun's runtime on "the `zeus-core` engine works", which the Ogun roadmap phrased as passing "Zeus segment I3". The Zeus roadmap no longer uses segment names, and "I3" is also invariant I3 of Zeus spec 03, so the gate is ambiguous. The Zeus daemon smoke test (`zeus/tools/smoke-daemon.mjs`) kills the daemon after 4,194,304 committed bytes of an 8,388,608-byte transfer, restarts it, confirms a resumed range with the original validator and verifies the file. It passed on 2026-10-18 on Windows. Fault injection at every crash point is still open Zeus phase 1 work.

The Zeus side also delivered what Ogun asked for in Zeus ADR 0011: the `service` client kind, `files.roots.register`, the service rules and a linking flow (Zeus spec 05 sections 7.1, 7.6, 9.6 and 15.2).

## Decision

1. **Ogun does not depend on `zeus-api`.** Ogun's session-bridge client speaks the Zeus daemon API as specified (Zeus spec 05: JSON-RPC 2.0 over the local endpoint) with its own small client crate, `ogun-zeus-bridge`, using only `zeus-core` crates (`zeus-types` for wire types). If a typed client becomes worth sharing, it moves to `zeus-core` as a thin crate with no app dependencies, through a Zeus ADR. Ogun's dependency rule is therefore: `zeus-core` crates only.
2. **The artifact store is `zeus-artifact`.** Ogun runs its own store instance through that crate. Content identity across products is the BLAKE3 digest of Zeus spec 13.
3. **The runtime gate is evidence, not a segment name.** Ogun's phase O1 may start because the Zeus kill-and-restart smoke test passes. Ogun's own O1 exit test (a partitioned backfill that survives `kill -9` of `ogund`) runs on Linux in Ogun's CI, so it covers a second platform. Zeus's fault-injection matrix stays a Zeus phase 1 exit item; Ogun tracks it but does not wait for it.
4. **Ogun's id prefixes** are proposed in Ogun spec O-00 and registered in Zeus spec 00 section 5.1 through a Zeus pull request. Until that lands, the O-00 table is the reservation, and no Ogun prefix may equal a Zeus prefix.

## Consequences

- The session bridge (phase O2) needs a hand-written client for a handful of spec 05 methods. That is less code than it sounds: Ogun calls `clients.*`, `files.roots.register`, job submission and events, and nothing else.
- Implementation can begin with the parts that need no bridge: the workflow IR, the run state machine, the run store and the embedded engine.
- ADR 0002 stays accepted; this record replaces only the statements listed above.

## Alternatives considered

- **Depend on `zeus-api` anyway:** rejected. It violates ADR 0002 decision 8 indirectly and couples Ogun's build to the consumer app's release cycle.
- **Split `zeus-api` into a types crate in `zeus-core` first:** reasonable, but it needs Zeus-side work and a Zeus ADR before Ogun needs the bridge. It remains open as the upgrade path in decision 1.
- **Keep waiting for the full fault-injection matrix:** rejected. It is a release criterion for Zeus, not a precondition for Ogun's design and first runtime code.
