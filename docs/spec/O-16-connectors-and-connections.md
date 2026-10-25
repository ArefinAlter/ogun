# O-16. Connectors, connections and exporters

| | |
|---|---|
| Status | Draft 1 (2026-10-24) |
| Depends on | O-02, O-03, O-05, O-14, O-15 |
| Decided by | ADR 0013 (Proposed) |
| Implemented by | `connectors/` (schema, catalog, `tools/check.mjs`); `integrations/export` (`ogun-export`); `apps/studio/web/src/connectors`, `views/settings` (Studio); the scripted demo backend `apps/studio/web/src/demo` implements sections 5 and 6 for demonstration only |

## 1. Purpose

This spec defines how Ogun describes the external systems it can reach, how a person configures a use of one, and
how Ogun sends telemetry out. It adds no way to run code, to approve, or to hold a secret.

## 2. The connector manifest

A manifest is a JSON file `connectors/catalog/<id>.connector.json` that validates against
`connectors/schema/connector.v0.schema.json`. Unknown members are refused.

| Member | Meaning |
|---|---|
| `connector_version` | `0`. |
| `id` | `^[a-z][a-z0-9-]{1,47}$`; equals the file name. |
| `name`, `vendor`, `summary`, `docs_url` | Words for people. `docs_url` is an `https` URL without a query. |
| `category` | `warehouse`, `database`, `object_storage`, `streaming`, `orchestration`, `observability`, `catalog`, `quality`, `notification`, `source`, `edge`, `agent`. |
| `regions` | `global`, `cn`, or both: where the vendor operates. |
| `maturity` | Section 3. |
| `directions` | `source` (Ogun reads), `sink` (Ogun writes), `emit` (Ogun sends telemetry or notices), `trigger` (the system calls Ogun), `edge` (devices push in). |
| `protocol` | `rest`, `s3_api`, `sql_wire`, `kafka_api`, `webhook`, `otlp`, `mqtt`, `cli`, `sdk`, `file`, `device`. |
| `auth` | Methods. Each has fields `{name, label, secret, required?, help?}`. A `secret` field is a name in the secret store, never a value. |
| `settings` | Plain values: `{name, label, type, required?, default?, options?, placeholder?, help?}`; `type` is `string`, `integer`, `boolean` or `enum`. |
| `egress` | `{domains, ports, note?}`. A domain may hold `{name}` placeholders that name a setting or an auth field of the same manifest. A connector that reaches outside the machine lists where; one that is only called, or reaches nothing, lists none. |
| `operators` | `{op, kind, summary, inputs?, outputs, determinism, via?, tool?}`. `op` is `vendor.verb@major`. `via` is `rest`, `cli` (with `tool`), `sdk`, `driver` or `builtin`. |
| `approvals` | Which approvals a workflow that uses it needs: `egress`, `code_change`, `budget`, `publish`. |
| `implementation` | `{path, evidence?}`: where the code is, for `built` and `verified`. |
| `notes` | Anything a person must know before using it. |

## 3. Maturity

| Value | Means | Requires |
|---|---|---|
| `contract` | The manifest only. No operator exists; nothing has been run against the system. | No `implementation`. |
| `built` | The code exists and passes its own tests. It has not run against the real service. | `implementation.path` that exists; evidence stating what was and was not checked. |
| `verified` | Run end to end against the real service. | `implementation.path`; evidence citing the run. |

## 4. Rules

`connectors/tools/check.mjs` fails on any of these, and `connectors/tools/check.test.mjs` shows each failing.

1. The schema in section 2, including closed enums and the `https` documentation URL.
2. A field named like a secret (O-02 section 10, and any name containing password, secret, token, API key, private
   key or credential) is declared `secret: true`.
3. A secret field has no default. A setting is never named like a secret.
4. Every `{placeholder}` in a domain names a setting or an auth field of the manifest.
5. Operator names are unique across the whole catalog. A `cli` adapter names its tool.
6. `built` and `verified` name code that exists; `contract` names none.
7. A connector that reaches a network port declares at least one domain, unless it is `trigger` or `edge` only.

## 5. Connections (proposed daemon methods)

A **connection** is `{connection_id (cnx_…), name, connector, auth, settings, secret_refs, state, last_test_at?,
last_test_ms?, last_error?, created_at, used_by}`. `secret_refs` maps an auth field to the *name* of a secret.
`name` is `^[a-z][a-z0-9_-]{0,63}$`, unique per daemon.

| Method | Parameters | Result | Who may call |
|---|---|---|---|
| `connections.list` | none | `connections` | human, agent, bridge |
| `connections.put` | `connection`, `idempotency_key` | the connection | human |
| `connections.delete` | `name`, `idempotency_key` | `{}` | human |
| `connections.test` | `name` | `{ok, elapsed_ms, checks[]}` | human |

`connections.test` checks, in order: the settings are complete; every secret the method needs is stored (the value
is not read); the hosts, with placeholders filled, are listed; and, for a connector with an operator, a minimal
read through that operator under the same sandbox and egress rules as a run. A test that would leave the machine
needs the connector's egress approval first. A `contract` connector has no operator, so its test stops after the
third check and says so.

The daemon methods `runtime.get` and `runtime.set` (run defaults and per-workflow overrides),
`environments.list`, `storage.rules` and `sources.*` that Studio's settings pages use are likewise proposals; the
daemon keeps them out of the closed O-15 set until each is specified.

## 6. Studio

Studio's Settings follow the sections of T3 Code's settings: General, Appearance, Keybindings, Providers,
Connectors, Connections, Schedules, Storage, Environments, Archive, with Evidence in place of snapshots and
Workflow sources in place of source control. A page whose daemon method is missing is shown read-only with the
words "this daemon does not ..."; the scripted build implements all of them.

- Studio shows secret **names** and the command to store one (`ogun secrets set <name>`); it has no field for a value.
- Appearance, keybindings and notification choices are device preferences, stored in the webview's own storage;
  the hosted web demo keeps them in memory only.
- The Providers page edits a copy of `llm.json` and validates it with the rules of O-03 section 8.1; the daemon
  stays the authority. Studio cannot write the file.

## 7. Exporters

`ogun-export` (`integrations/export`) is a separate process with an `agent` credential. It reads `daemon.status`,
`runs.get`, `events.since`, `approvals.list` and `lineage.get`, and sends:

- gauges `ogun.workflows`, `ogun.runs.active`, `ogun.approvals.pending`, `ogun.streams` and `ogun.node_instances`
  (labelled by state);
- events for failed node instances, retries, a run finishing and an approval waiting for a person;
- OpenLineage run events for finished runs.

Delivery is at least once: a batch is written to the sink's outbox before the send, removed after success, and the
event cursor advances only after every sink's batch is on disk. Secrets come from `{"env": NAME}` references; a
literal secret in a config file is refused without echoing it. An exporter cannot approve, store a workflow, or
reach a secret.

Sinks: Datadog, OpenLineage (Marquez's path by default), Slack, Microsoft Teams, PagerDuty, DingTalk, Feishu and
Lark, WeCom, a generic signed webhook, Splunk HEC, Prometheus Pushgateway, Elasticsearch, OTLP/HTTP, New Relic and
Loki. All were checked against local fakes written from public documentation, none against the real service.

## 8. Conformance

```sh
node connectors/tools/check.mjs                      # the catalog passes section 4
node --test connectors/tools/check.test.mjs          # and each rule can fail
cd integrations/export && PYTHONPATH=src python -m unittest discover -s tests
```

## 9. Open issues

1. The id prefix `cnx_` (connection) is not yet in O-00's table or Zeus 00 section 5.1.
2. Each connector's operators need entries in the IR catalogue before a workflow can name them (ADR 0003).
3. Whether `connections.test` for a `built` connector should require the egress approval at test time or accept
   the connector's standing approval.
4. A Rust `ogun-connectors` crate that embeds the catalog, so the daemon, `ogun lsp` and `ogun mcp` read the same
   manifests Studio does.
5. Vendor facts (domains, ports, regions, auth methods) were written from public documentation by several authors
   and have not been exercised; each needs an owner before a connector leaves `contract`.
