# ADR 0013: Connectors are manifests; connections hold names, never secrets

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-24 |
| Decided by | Pending the maintainer |
| Refines | ADR 0004 (integrate, do not replace) and ADR 0012 (data platforms call Ogun; data and lineage flow out) |
| Specifies | O-16 |

## Context

Teams run Ogun next to warehouses, object stores, streams, observability tools and chat systems, many of them in
China (Alibaba Cloud, Tencent Cloud, Huawei Cloud, Volcano Engine, DingTalk, Feishu, WeCom). ADR 0004 says
integrations are operators behind adapters, and ADR 0012 fixes the shape of the orchestrator and lineage
integrations. Neither says how a person sees which systems Ogun can reach, what each one needs (hosts to approve,
secrets to store, settings to fill), and how far the code behind it has come. Without that, "does Ogun work with
X" has no honest answer, and every integration is rediscovered by whoever needs it.

## Decision

1. **A connector is a manifest.** One JSON file per external system, validated by `connectors/schema/connector.v0.schema.json`.
   It names the system, its category, the auth methods it supports, its settings, the hosts it contacts, the
   operators it would contribute, and what a person must approve. It holds no secret and no value that belongs to
   one person.
2. **Maturity is stated, not implied.** `contract`: the manifest only. `built`: the code exists and passes its own
   tests; it has not run against the real service. `verified`: run end to end against the real service, with the
   evidence cited. A manifest that is `built` or `verified` names the code and the test. Nothing is `verified` today.
3. **A connection is a named use of a connector.** Plain settings, plus the *names* of the secrets that hold the
   rest. Secret values are stored on a terminal with `ogun secrets set`; Studio never asks for, shows or stores one.
4. **Secrets in manifests are a validation error.** A field named like a secret (O-02 section 10) must be declared
   `secret: true`; a secret field has no default; a setting cannot be named like a secret; every domain placeholder
   names a setting or field of the same manifest. A test enforces each rule.
5. **New operators enter the closed catalogue one at a time.** A connector's operators are proposals until each is
   added to the IR catalogue by an accepted change (ADR 0003 keeps the operator set closed). Until then
   `ogun validate` refuses a workflow that names one, and Studio says so beside every example.
6. **Outbound telemetry is a separate process.** `ogun-export` follows the daemon's events with an `agent`
   credential and sends metrics, events and lineage to Datadog, Splunk, Elastic, OpenTelemetry, New Relic, Loki,
   Prometheus, Marquez-style OpenLineage endpoints and chat or paging systems. It reads secrets from its own
   environment, delivers at least once through an on-disk outbox, and cannot decide an approval.
7. **Connections, run defaults and similar settings are daemon state behind new methods**, proposed in O-16 and
   not yet in O-15. Until a daemon offers them Studio shows the matching page read-only with an honest notice.

## Consequences

- The catalog can be wrong about a vendor. Each manifest is written from public documentation and says `contract`
  until someone runs it; a wrong domain or port is a one-line fix reviewed in a pull request.
- Breadth costs review: about a hundred manifests need an owner for each vendor family. The checks keep the form
  consistent; they cannot keep the facts current.
- Studio, the language server and the MCP tools can all read the same catalog, so what an agent proposes and what a
  person sees agree.
- The exporter adds a second place where secrets are handled (the environment of a process). It never logs or
  returns one; tests plant a canary value and look for it.

## Alternatives considered

- **Hard-code connectors in Studio:** rejected. The facts would be invisible to the CLI, the language server and
  agents, and could not be reviewed as data.
- **A plugin system that loads vendor code:** rejected for now. ADR 0004 prefers sandboxed adapters over tools the
  vendor already ships; a plugin loader is a second trust boundary.
- **Store secrets in connections:** rejected. It would put values into a second store, and Studio would have to
  handle them. Constraint 4 is easier to keep when no screen takes a secret.
