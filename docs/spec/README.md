# Ogun specifications

Normative contracts for Ogun. The key words MUST, SHOULD and MAY are used as in RFC 2119. Prose wins over schema; a schema that disagrees with its spec is a bug in the schema.

Ogun adopts the Zeus specifications by reference where it says so, in particular Zeus spec 00 (conventions). A Zeus spec is cited as "Zeus 00 section 7"; an Ogun spec as "O-02 section 8".

| Spec | Title | Status | Implemented by |
|---|---|---|---|
| [O-00](O-00-conventions.md) | Conventions delta | Draft 1 | `ogun-types` |
| [O-01](O-01-domain-model-and-run-state-machine.md) | Domain model and run state machine | Draft 1 | `ogun-run`, `ogun-store`, executor in `ogun-daemon` |
| [O-02](O-02-workflow-ir-v0.md) | Workflow IR v0 | Draft 1 | `ogun-ir`, [`schema/workflow-ir.v0.schema.json`](schema/workflow-ir.v0.schema.json) |
| [O-03](O-03-operator-abi-v0.md) | Operator ABI v0 | Draft 1 | catalogue in `ogun-ir`; registry, core operators, runner and sandbox in `ogun-ops`; tool shims in `ogun-op-shims`; model operators in `ogun-llm`, `ogun-op-llm` |
| [O-04](O-04-scheduling.md) | Scheduling: triggers, partitions, backfills, budgets | Draft 1 | `ogun-sched` |
| O-05 | Lineage and run records | Planned | |
| O-06 | Benchmark profile | Planned | |
| [O-07](O-07-approvals-and-trust-surfaces.md) | Approvals and trust surfaces | Draft 1 | `ogun-daemon`, `ogun approve` |
| O-08 | Session bridge | Planned | |
| [O-09](O-09-streams-and-windowed-partitions.md) | Streams and windowed partitions | Draft 1 | `ogun-stream`; IR rules in `ogun-ir` |
| [O-10](O-10-edge-ingestion.md) | Edge ingestion and the ingest log | Draft 1 | `ogun-edge::log`, `ogun-ingest` |
| [O-11](O-11-edge-hub-sync.md) | Edge-to-hub sync | Draft 1 | `ogun-edge::sync`, `ogun-sync` |
| [O-12](O-12-platform-profiles.md) | Platform profiles: the support matrix | Draft 1 | CI matrix |
| O-13 | Server mode: users, workspaces, roles, OIDC, WebAuthn approvals | Planned | |
| [O-14](O-14-extension-and-integration-surfaces.md) | Extension and integration surfaces | Draft 1 | `ogun mcp`, `extensions/vscode` |
| [O-15](O-15-daemon-api.md) | The `ogund` local API | Draft 1 | `ogun-api`, `ogun-daemon` |

Draft 1 means: complete enough to implement against, not yet reviewed line by line by the maintainer. The ADRs they rest on (0002 to 0012) are accepted.

Conformance: `cargo test --workspace` validates every example in `examples/` against the JSON Schema and the validator, and checks that the O-01 transition table in prose matches the table in code.
