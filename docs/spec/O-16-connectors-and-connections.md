# O-16. Connectors, connections and exporters

| | |
|---|---|
| Status | Draft 1 (2026-10-24) |
| Depends on | O-02, O-03, O-05, O-14, O-15 |
| Decided by | ADR 0013 (Accepted by the maintainer's instruction of 2026-10-26); ADR 0015 decision 10 (a secret value may be typed into the desktop window); ADR 0016 (`export@1`) |
| Implemented by | `connectors/` (schema, catalog, `tools/check.mjs`); `integrations/export` (`ogun-export`); `crates/ogun-connectors` (the catalogue compiled into Rust); `crates/ogun-daemon/src/studio` (`connections`, `runtime`, `environments`, `storage`, `ladder`: the methods of section 5, served by `ogund`); `crates/ogun-ops/src/export` (the drivers behind the probe and `export@1`); `apps/studio/web/src/connectors`, `views/settings` (Studio); the scripted backend `apps/studio/web/src/demo` still implements sections 5 and 6 for the hosted web build |

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

## 5. Connections (daemon methods)

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

### 5.1 What `ogund` implements (0.0.1b)

`ogund`, Studio's own engine (ADR 0015), serves the methods below; O-15 section 5.1 says who may call each. Everything
here is the daemon's own state, small JSON documents under `<state>/settings/` or the run and artifact stores it already
keeps; none of it can decide an approval, read a secret value or run code.

- **`connections.*`** as in the table above. `connections.put` validates against the manifest in `ogun-connectors` (the
  catalogue compiled into the daemon): the connector, the authentication method, each
  setting's type and options, and that every `secret_refs` value is a valid secret **name**. A setting that looks like a
  secret value, or a secret reference that does, is refused with `api.invalid_params`, not masked. Saving sets the state to
  `untested`. `connections.list` adds `used_by`, the workflows whose `export` nodes name the connection. A connection
  holds no secret value (ADR 0013, ADR 0015 decision 10 for how a value gets into the secret store).
- **`connections.test`** runs the four checks of the table above. The fourth, the probe, runs when this build has a
  driver for the connector (`local-files`, the S3 family and `sqlite`, ADR 0016): it does what a delivery would do and none
  of the person's data (list one object, write and delete a probe file, create and drop a scratch table). For any other
  connector it stops after the manifest checks and the result says `simulated: true`. A test is a person's button press
  and does not ask for the connector's egress approval first (open issue 3). The result updates the connection's `state`
  (`healthy` or `failing`), `last_test_at`, `last_test_ms` and `last_error`, which is a joined sentence of the failed checks.
- **`runtime.get`, `runtime.set`, `runtime.reset`, `scheduler.set_enabled`**: run defaults (concurrency, retries, budgets,
  approval expiry, model access, auto-pause after failures, auto-archive, resume on start) and per-workflow overrides.
  `runtime.get` reports which members the daemon applies (`enforced`: `concurrency`, `approval_expiry_hours`,
  `model_access`, `auto_pause_after_failures`), so Studio never shows a stored setting as active when nothing reads it.
- **`environments.list`**: `{environments[], bridges[], scheduler_enabled}`. "This computer" is always first, then enrolled
  edges. The bridges are the Zeus session bridge, the ingest bridge (running when it authenticated within five minutes),
  edge sync, the server, and remote approvals, which always read `locked` (O-13 section 12).
- **`sandbox.status`** (O-03 section 7): the backend, and in words what it isolates and does not.
- **`evidence.get`, `evidence.set`**: what a run's evidence bundle contains (`enabled`, `format` `json` or `html`,
  `include_lineage`, `include_previews`). Studio assembles the bundle itself from `runs.get`, `artifacts.*` and
  `lineage.get` and hashes the document; nothing is uploaded.
- **`storage.info`, `storage.rules`, `storage.set_rules`, `storage.set_retention`, `storage.cleanup_plan`,
  `storage.cleanup`, `storage.clear_caches`**: what the artifact store holds, and rules in days (retention, failed,
  cancelled, partial outputs, caches; `0` is off and every removal rule is off by default; `keep_lineage_referenced` keeps
  the newest successful output of each partition). `storage.cleanup_plan` says what a cleanup would remove; `storage.cleanup`
  then removes the artifact records of the runs the rules select (the store tombstones them and collects the content nothing
  else references) and clears the caches. A run's record, events and lineage stay: a finished partition stays finished, the
  run page still says what ran, and a later run of the partition makes the bytes again. The size limit (`limit_gib`) is the
  artifact store's quota, applied when the engine starts.
- **`ladder.get`, `ladder.put`**: the provider ladder (`llm.json`, O-03 section 8.1) and the month's spend from the ledger.
  `ladder.put` refuses a provider list that looks like it holds a key (a provider names its key's secret in
  `api_key_secret`, never the key).

**Not served.** Studio's shell may relay `sources.*`, `schedules.*`, `archive.*` and `evidence.bundle`, but `ogund` has no
row for them and answers `api.method_not_found`; the pages for workflow sources, schedules and the archive show "this
engine does not ..." and are read-only.

## 6. Studio

Studio's Settings follow the sections of T3 Code's settings: General, Appearance, Keybindings, Providers,
Connectors, Connections, Schedules, Storage, Environments, Archive, with Evidence in place of snapshots and
Workflow sources in place of source control. A page whose daemon method is missing is shown read-only with the
words "this daemon does not ..."; the scripted backend of the hosted web build implements all of them.

- Studio shows secret **names**. On the desktop, with its own engine, a masked field next to a name takes the value and sends it
  once, through the shell, to `secrets.put` (ADR 0015 decision 10); the field is cleared at once and no page ever shows,
  returns or keeps a value. Studio-web and a hub link have no such field: use `ogun secrets set <name>` on the machine that
  runs the engine.
- Appearance, keybindings and notification choices are device preferences, stored in the webview's own storage;
  the hosted web build keeps them in memory only.
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

1. The id prefix `cnx_` (connection) is in O-00's table; it is not yet in Zeus 00 section 5.1 (a Zeus pull request).
2. Each connector's operators need entries in the IR catalogue before a workflow can name them (ADR 0003). `parse@1` and `export@1` entered it by ADR 0016; `export@1` delivers to `local-files`, the S3 family and `sqlite` only.
3. Whether `connections.test` for a `built` connector should require the egress approval at test time or accept
   the connector's standing approval.
4. The `ogun-connectors` crate embeds the catalog and the daemon reads it; `ogun lsp` and `ogun mcp` do not yet.
5. Vendor facts (domains, ports, regions, auth methods) were written from public documentation by several authors
   and have not been exercised; each needs an owner before a connector leaves `contract`.
