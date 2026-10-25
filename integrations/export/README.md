# ogun-export

Sends run, partition and approval information out of Ogun to observability, lineage and chat systems: Datadog, OpenLineage servers, Slack, Microsoft Teams, PagerDuty, DingTalk, Feishu and Lark, WeCom, a generic signed webhook, Splunk, Prometheus, Elasticsearch, OpenTelemetry, New Relic and Grafana Loki.

It follows the `ogund` event stream with an `agent` credential, maps what it sees, writes it to a durable outbox per destination, and delivers from there. It is read-only and it is standard library only (the command line imports [`ogun-client`](../python/ogun-client) when it connects, and nothing else does).

```sh
pip install ./integrations/python/ogun-client ./integrations/export
```

## Read this first

**The sinks were checked against local fakes written from public documentation. They were not checked against the real services.** Every request path, header and body shape here comes from each vendor's published API description, and every test runs against a fake HTTP server on `127.0.0.1`. The section [Not verified against the real service](#not-verified-against-the-real-service) lists what I am least sure of. Run `ogun-export check`, then point one sink at a test project or channel before you rely on it.

## An agent credential, and never an approval

The exporter authenticates with a local credential of kind `agent`. A person creates it once on a terminal of the machine that runs `ogund`:

```sh
ogun credentials add export --kind agent
```

The credential is read from Ogun's state directory as the user the exporter runs as. The client refuses any other kind, and the command line checks again after connecting.

**The exporter can never approve anything.** It calls exactly six daemon methods, all read-only: `status`, `list_runs`, `get_run`, `events_since`, `list_approvals` and `lineage`. It has no path to `approvals.decide`, to credentials, secrets, approver keys or edges; `ogund` would refuse them for an agent (O-15 section 6), and the exporter's own client wrapper does not expose them. What it does with approvals is tell people: a pending approval becomes an `attention` event ("waiting for a person"), and a person decides it on the Ogun CLI or in Ogun Studio.

## Quick start

1. Create the credential (above).
2. Put every secret in the environment of the process, never in the file.
3. Write a configuration file.
4. `ogun-export check --config export.json`
5. `ogun-export run --config export.json`

```json
{
  "version": 1,
  "credential": "export",
  "ogun_home": null,
  "profile": null,
  "state_dir": "~/.ogun-export",
  "interval_s": 15,
  "snapshot_interval_s": 60,
  "sinks": [
    {"name": "dd", "type": "datadog", "site": "datadoghq.com", "api_key": {"env": "DD_API_KEY"}, "tags": ["env:prod"], "min_severity": "warning"},
    {"name": "chat", "type": "slack", "webhook_url": {"env": "SLACK_WEBHOOK_URL"}},
    {"name": "lineage", "type": "openlineage", "url": "https://marquez.internal:5000", "auth": {"env": "MARQUEZ_TOKEN"}}
  ]
}
```

| Key | Meaning |
|---|---|
| `version` | Must be `1`. |
| `credential` | Name of the `agent` credential (default `export`). |
| `ogun_home`, `profile` | Choose the daemon, as `$OGUN_HOME` and the profile do for the `ogun` CLI. `null` means the default discovery. |
| `state_dir` | Cursor, outbox and bookkeeping (default `~/.ogun-export`). Run one exporter per state directory. |
| `interval_s` | The longest the loop waits on the daemon before it retries undelivered batches and checks whether a snapshot is due. At most 25 seconds are used as the long-poll wait. Default 15. |
| `snapshot_interval_s` | How often daemon counters are sent and pending approvals are checked. Default 60. |
| `sinks` | One object per destination: `name` (unique, `^[a-z][a-z0-9_-]{0,31}$`), `type`, and the keys of that type. |

**Unknown keys are errors**, at the top level and in every sink; the message names the key, never a value. **A member that holds a secret must be `{"env": "NAME"}`.** That covers `api_key`, `token`, `password`, `secret`, `webhook_url`, `routing_key`, `signing_secret`, `key`, `auth` and `header_value_env`, and any name ending in `_key`, `_token`, `_password` or `_secret`. A literal string there is refused with `<path>: a secret must be {"env": "NAME"}` and the literal is not printed. A missing or empty variable is an error that names the variable. Values must be printable ASCII.

URLs given in the file (`url`, `endpoint`) must not carry credentials, a query string or a fragment. Plain `http` is accepted only for loopback hosts, unless the sink sets `allow_insecure_http: true`. Redirects are never followed, because a redirect would resend headers that may carry a secret.

Every sink also takes `timeout_s` (1 to 120, default 10). Sinks that send events take `min_severity`.

## Commands

| Command | What it does |
|---|---|
| `ogun-export run --config FILE` | Follows the daemon until stopped (SIGINT or SIGTERM; Ctrl+Break on Windows). Waits for the daemon if it is not up yet and reconnects after it restarts. |
| `ogun-export run --config FILE --once` | One pass, then exit. Exit code 1 if the daemon could not be reached or any destination still has undelivered batches. |
| `ogun-export check --config FILE [--offline]` | Validates the file, confirms each environment variable is set (it prints `set` or `NOT SET`, never a value), prints a table of sinks with their capabilities, minimum severity and the host names each will contact, and then connects to the daemon and reads `status` (skip with `--offline`). Use the host list for an egress approval or a firewall rule. |
| `ogun-export sinks` | The catalogue: every type with its description, capabilities, egress hosts and configuration keys. |

Exit codes: 0 success, 1 runtime failure, 2 usage or configuration error. `-v` before the command turns on debug logging. Logs go to standard error and contain messages, ids and counts; never a body, header, URL query or secret.

## What is sent

The daemon's events become small records. Most are not worth a message, so a destination receives only what its `min_severity` allows.

| What happened | Severity | Sent by default |
|---|---|---|
| A node instance moved to `failed` (any transition to `failed`) | `error` | yes |
| A node instance moved to `retry_scheduled` | `warning` | yes |
| A node instance moved to `waiting_for_user` | `attention`: a person must act | yes, always |
| A run reached `succeeded`, `failed` or `cancelled` (`run_finished`, once per run) | `info`, `error`, `info` | yes, always |
| An approval became pending | `attention` | yes, always |
| A node instance that had alerted later `succeeded` (`node_recovered`); an approval stopped being pending (`approval_resolved`) | `info` | only PagerDuty (to resolve), or `min_severity: info` |
| Everything else: `run_created`, other transitions, event types this version does not know | `info` | only with `min_severity: info` |

`min_severity` is `info`, `warning` (the default) or `error`. `attention` events and `run_finished` always pass. PagerDuty ignores `min_severity` (it triggers on `error` and `attention` only).

An unknown event type, a missing key, a key of the wrong type or an extra key never raises; it becomes a generic `info` event. A `run_finished` is detected by asking the daemon (`get_run`) after a node or run reached a terminal state, so it is sent once per run even if several terminal events arrive. The workflow, node and partition names come from `get_run`; a run the daemon no longer knows is still exported, with ids only. The deduplication key of a node event is `run_id:node:partition`.

Daemon counters (`ogun.workflows`, `ogun.runs.active`, `ogun.approvals.pending`, `ogun.streams`, and `ogun.node_instances` with a `state` label) are sent every `snapshot_interval_s` to destinations that take metrics. Node instances are counted over runs that have not finished, among the 100 newest runs, so they are a gauge of current work and not a history. On the first start the exporter begins at the daemon's current `last_event_seq`; it does not replay history. For every run that finishes, its OpenLineage events (`lineage`) are read once and sent to destinations that take lineage; the last 1000 run ids are remembered in `state_dir/lineage-sent.json`.

## At-least-once delivery and the outbox

Delivery is at least once, across failures and restarts, and in order for each destination.

- For every destination there is a directory `state_dir/outbox/<sink name>/` with one JSON file per pending batch, named by a growing zero-padded counter.
- A batch is written to disk atomically (a temporary file, then `os.replace`) **before** any send is attempted, and deleted only after the send succeeded.
- The cursor (`state_dir/cursor.json`) is saved **after** the batch is in every interested outbox. A crash between the two re-sends events; it never loses them.
- Draining retries the oldest file first and stops at the first failure, so order is kept and one destination being down never blocks another. A destination that failed is retried with backoff (`interval_s`, doubling, at most five minutes) by the loop; `--once` always tries.
- A batch for a destination that sends one request per item (Slack, Teams, PagerDuty, DingTalk, Feishu, WeCom, Datadog events, OpenLineage) records how far it got, and the file is rewritten without the items already delivered, so a restart does not repeat them.
- At most 1000 files wait per destination. Beyond that the oldest are dropped and counted (`dropped` in the status, kept in `outbox/<sink>/.meta.json`).
- A duplicate can happen: after a crash between a successful send and the deletion of its file, or between saving a batch and the cursor. Elasticsearch ignores duplicates (stable document ids), PagerDuty and Datadog fold them by `dedup_key`, and the webhook and Splunk receivers should tolerate them.

Files in the state directory: `cursor.json`, `open-alerts.json` (node instances that alerted, so a later success can resolve them), `lineage-sent.json`, `approvals-pending.json` and `outbox/`. They contain event text (workflow, node and run ids, causes) but never secrets.

## Destinations

All HTTP is plain `urllib`. Connection errors, `429` and `5xx` are retried three times with backoff of 0.5, 1 and 2 seconds, and `Retry-After` is honoured up to 60 seconds. Other `4xx` are never retried. A delivery error names the HTTP status, the sink and the URL without userinfo, query or fragment (for a webhook whose URL is the secret, without the path as well). It never contains a body, a header or a secret.

| Type | Takes | Contacts | Secrets (read from the environment) |
|---|---|---|---|
| `datadog` | events, metrics | `api.<site>` | `api_key` |
| `openlineage` | lineage | host of `url` | optional `auth`, `header_value_env` |
| `slack` | events | host of the webhook URL (`hooks.slack.com`) | `webhook_url` |
| `teams` | events | host of the webhook URL (depends on the tenant) | `webhook_url` |
| `pagerduty` | events | `events.pagerduty.com`, or `events.eu.pagerduty.com` | `routing_key` |
| `dingtalk` | events | `oapi.dingtalk.com` | `webhook_url`, optional `secret` |
| `feishu` | events | `open.feishu.cn` or `open.larksuite.com` | `webhook_url`, optional `secret` |
| `wecom` | events | `qyapi.weixin.qq.com` | `webhook_url` |
| `webhook` | events, metrics, lineage | host of `url` | optional `signing_secret`, `headers_env` |
| `splunk` | events, metrics | host of `url` | `token` |
| `prometheus` | metrics | host of `url` (a Pushgateway) | optional `username`, `password` |
| `elastic` | events, metrics | host of `url` | `api_key` |
| `otlp` | events (as logs), metrics | host of `endpoint` | optional `headers_env` |
| `newrelic` | events (as logs), metrics | `metric-api.newrelic.com`, `log-api.newrelic.com` (`.eu.` for region `eu`) | `api_key` |
| `loki` | events | host of `url` | optional `token`, or `username` and `password` |

For the webhook sinks the host is the host of the URL in the variable, so `check` prints the host you configured and a documented default only when the variable is not set. The variable names below are examples; the name is whatever you write in `{"env": ...}`.

### Datadog

`site` is one of `datadoghq.com` (default), `datadoghq.eu`, `us3.datadoghq.com`, `us5.datadoghq.com`, `ap1.datadoghq.com`, `ddog-gov.com`. Events go to `POST /api/v1/events` with the `DD-API-KEY` header; gauges go to `POST /api/v2/series` (type 3, a gauge) with a `host` resource. Tags are `service:ogun`, your `tags`, `workflow:<w>` and `node:<n>`. Optional `host` overrides the machine name attached to metrics.

```json
{"name": "dd", "type": "datadog", "site": "datadoghq.eu", "api_key": {"env": "DD_API_KEY"}, "tags": ["env:prod"], "min_severity": "warning"}
```

### OpenLineage

`POST <url><path>` once per OpenLineage run event, `Content-Type: application/json`. The default `path` is `/api/v1/lineage`, **which is Marquez's**. Other servers (DataHub, OpenMetadata, Atlan and others) take OpenLineage events on other paths and with other authentication. Check the path against that vendor's documentation and set `path` yourself; this package does not guess one. `auth` is sent as `Authorization: Bearer <value>`. For a token in another header, set `header_name` and `header_value_env` together.

```json
{"name": "marquez", "type": "openlineage", "url": "https://marquez.internal:5000", "path": "/api/v1/lineage", "auth": {"env": "MARQUEZ_TOKEN"}}
```

### Slack

An incoming webhook. The URL is the secret, so it comes from the environment. Each event is one message: a `text` fallback plus a section block and a context block. `&`, `<` and `>` are escaped as Slack asks.

```json
{"name": "chat", "type": "slack", "webhook_url": {"env": "SLACK_WEBHOOK_URL"}, "min_severity": "error"}
```

### Microsoft Teams

A Workflows (Power Automate) webhook that accepts a message with an Adaptive Card attachment (version 1.4): a title, the text and a fact set.

```json
{"name": "teams", "type": "teams", "webhook_url": {"env": "TEAMS_WEBHOOK_URL"}}
```

### PagerDuty

Events API v2 at `https://events.pagerduty.com/v2/enqueue` (`region: "eu"` uses `events.eu.pagerduty.com`). Only severity `error` and `attention` trigger, with `dedup_key = run_id:node:partition` (`approval id` for approvals, the run id for a failed run). When the same node instance later reaches `succeeded`, or the approval stops being pending, the exporter sends `resolve` with the same `dedup_key`. A failed run also triggers one incident for the run, separate from the incidents of its failed nodes. `attention` is sent as PagerDuty severity `warning`.

```json
{"name": "pd", "type": "pagerduty", "region": "us", "routing_key": {"env": "PD_ROUTING_KEY"}}
```

### DingTalk

A group robot webhook, markdown messages. With `secret` the URL gets `&timestamp=<ms>&sign=<url-encoded base64 of HMAC-SHA256>` appended (key: the secret; message: `<timestamp>\n<secret>`). DingTalk answers HTTP 200 with `{"errcode": N, ...}`; a non-zero `errcode` is a delivery failure and only the number is reported. Every title starts with `Ogun`, for robots set to keyword security.

```json
{"name": "ding", "type": "dingtalk", "webhook_url": {"env": "DINGTALK_WEBHOOK_URL"}, "secret": {"env": "DINGTALK_SECRET"}}
```

### Feishu and Lark

A custom bot webhook, text messages. With `secret` the body gains `timestamp` (Unix seconds as a string) and `sign` (base64 of HMAC-SHA256 keyed with `<timestamp>\n<secret>` over an empty message). Success is `{"code": 0}` (older bots answer `StatusCode`); anything else is a failure, and only the number is reported.

```json
{"name": "feishu", "type": "feishu", "webhook_url": {"env": "FEISHU_WEBHOOK_URL"}, "secret": {"env": "FEISHU_SECRET"}}
```

### WeCom

A group robot webhook, markdown messages cut to 4000 bytes. Success is `errcode == 0`.

```json
{"name": "wecom", "type": "wecom", "webhook_url": {"env": "WECOM_WEBHOOK_URL"}}
```

### Generic webhook

`POST <url>` with `{"type": "events" | "snapshot" | "lineage", "sent_at": <RFC 3339>, "items": [...]}` (events and lineage, at most 100 items per request) or `"snapshot": {...}`. With `signing_secret` every request carries `X-Ogun-Signature: sha256=<hex>`, the HMAC-SHA256 of the exact body bytes. `headers_env` maps header names to environment references, for example an `Authorization` header. `Content-Type`, `Content-Length`, `Host`, `User-Agent` and the signature header cannot be set.

```json
{"name": "hook", "type": "webhook", "url": "https://hooks.internal/ogun", "signing_secret": {"env": "OGUN_HOOK_SECRET"}, "headers_env": {"Authorization": {"env": "OGUN_HOOK_AUTH"}}}
```

### Splunk

HTTP Event Collector: `POST <url>/services/collector/event`, `Authorization: Splunk <token>`, a body of concatenated JSON objects `{"time", "host", "source": "ogun", "sourcetype": "ogun:event" | "ogun:snapshot", "index"?, "event": {...}}`. The snapshot is sent as one event of sourcetype `ogun:snapshot`, not to the metrics endpoint. Success is `{"text": "Success", "code": 0}`. There is no option to switch certificate verification off; give Splunk a certificate the machine trusts.

```json
{"name": "splunk", "type": "splunk", "url": "https://splunk.internal:8088", "token": {"env": "SPLUNK_HEC_TOKEN"}, "index": "ogun"}
```

### Prometheus

Pushes gauges to a Pushgateway with `PUT <url>/metrics/job/<job>[/instance/<instance>]`, text exposition format 0.0.4: `ogun_workflows`, `ogun_runs_active`, `ogun_approvals_pending`, `ogun_streams` and `ogun_node_instances{state="..."}`. A `PUT` replaces the whole group, which is what gauges need. `job` (default `ogun`) and `instance` may contain letters, digits and `_ . : -` only. The exporter opens no listener of its own.

```json
{"name": "prom", "type": "prometheus", "url": "http://pushgateway.internal:9091", "job": "ogun", "instance": "plant-1", "username": {"env": "PUSH_USER"}, "password": {"env": "PUSH_PASSWORD"}}
```

### Elasticsearch

`POST <url>/_bulk` with NDJSON: a `create` action and a document with `@timestamp` per event, into `index` (default `ogun-events`); snapshots go to `metrics_index` (default `ogun-metrics`). Each document has a stable `_id`, so a re-sent batch creates nothing twice and a `409` counts as delivered. `errors: true` in the reply is a failure, and the message reports counts only. Authentication is `Authorization: ApiKey <value>`: use the `encoded` value Elasticsearch shows when it creates the key. A document the cluster permanently rejects (for example a mapping conflict) keeps its batch in the outbox until the cause is fixed.

```json
{"name": "es", "type": "elastic", "url": "https://es.internal:9200", "api_key": {"env": "ES_API_KEY"}, "index": "ogun-events", "metrics_index": "ogun-metrics"}
```

### OpenTelemetry

OTLP over HTTP with JSON bodies: events as log records to `POST <endpoint>/v1/logs`, the snapshot as gauges to `POST <endpoint>/v1/metrics`. The resource carries `service.name=ogun`. A reply that reports a partial success is logged with the count and treated as delivered, because OTLP clients must not retry rejected records. `headers_env` adds headers such as an API key.

```json
{"name": "otel", "type": "otlp", "endpoint": "http://localhost:4318", "headers_env": {"x-api-key": {"env": "OTLP_API_KEY"}}}
```

### New Relic

Logs to `POST https://log-api.newrelic.com/log/v1` and gauges to `POST https://metric-api.newrelic.com/metric/v1` (`region: "eu"` uses the `.eu.` hosts), authenticated with the `Api-Key` header, with `service.name=ogun` as a common attribute.

```json
{"name": "nr", "type": "newrelic", "region": "us", "api_key": {"env": "NEW_RELIC_LICENSE_KEY"}}
```

### Grafana Loki

`POST <url>/loki/api/v1/push`, one stream per level and workflow with the labels `service_name`, `level` and `workflow`, and a JSON line per event. Authentication is a bearer `token` or `username` and `password`; `tenant_id` sets `X-Scope-OrgID`. Loki refuses entries older than its configured age limit, so events held back for a long time by an unreachable Loki can be rejected when they finally arrive.

```json
{"name": "loki", "type": "loki", "url": "https://loki.internal:3100", "token": {"env": "LOKI_TOKEN"}, "tenant_id": "ogun"}
```

## Running it as a service

The exporter must run as the user that owns the credential and can reach `ogund`'s socket. Put the secrets in an environment file that only that user can read.

### Linux (systemd)

`/etc/systemd/system/ogun-export.service`:

```ini
[Unit]
Description=Ogun exporter
After=network-online.target
Wants=network-online.target

[Service]
User=ogun
EnvironmentFile=/etc/ogun-export/env
ExecStart=/usr/local/bin/ogun-export run --config /etc/ogun-export/export.json
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

`/etc/ogun-export/env` holds `NAME=value` lines and is `chmod 600`, owned by `ogun`. Then `systemctl enable --now ogun-export`. Set `state_dir` to a directory that user owns, for example `/var/lib/ogun-export`.

### Windows (scheduled task)

Set the secrets as environment variables of the user the task runs as (System Properties, Environment Variables, user variables), and then register a task that starts at logon and restarts on failure:

```powershell
$action   = New-ScheduledTaskAction -Execute "ogun-export.exe" -Argument 'run --config C:\ProgramData\ogun-export\export.json'
$trigger  = New-ScheduledTaskTrigger -AtLogOn
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero)
Register-ScheduledTask -TaskName "Ogun Export" -Action $action -Trigger $trigger -Settings $settings
```

Instead of a long-running task, a cron entry or a task that runs `ogun-export run --config FILE --once` every minute also works: the cursor and the outbox live on disk, so nothing is lost between runs. Each `--once` pass takes a snapshot.

## Tests

```sh
cd integrations/export
PYTHONPATH=src python -m unittest discover -s tests -v
```

The suite is hermetic: a fake HTTP server on `127.0.0.1` with an ephemeral port, a stub with the methods of `ogun_client.Client`, no installed dependencies, and no real sleeping (the retry backoff and the loop's pauses are injected). It checks each sink's path, method, headers and body, the error-code replies, retries and `Retry-After`, the outbox and restart behaviour, tolerant mapping, `run_finished` and lineage once per run, PagerDuty trigger and resolve, configuration validation, the command line, and that a canary secret never appears in an exception, a `repr`, a log record, an outbox file or the output of `check`.

## Not verified against the real service

None of these destinations was contacted. These are the points where I relied on my reading of public documentation and am least sure; each is a place to look first if a real service rejects a request.

- **Datadog.** Title limit of 100 characters and text limit of 4000, and an `aggregation_key` of at most 100, are applied from memory of the events API. Series `type` 3 is the gauge. Whether your site accepts the host `resources` entry as written is untested. Only the events and series endpoints are used.
- **OpenLineage.** Only Marquez's path is the default and even that is unconfirmed. DataHub, OpenMetadata, Atlan and others are not supported by default.
- **Slack.** Block Kit section and context blocks are the simplest forms; the reply body (`ok`) is not required, any `2xx` is success. Slack Workflows webhooks other than incoming webhooks are untested.
- **Teams.** Workflows webhook flows differ by tenant and by the flow's schema; some expect the card in a different envelope or reject version 1.4. The shape here is the commonly documented Adaptive Card message.
- **PagerDuty.** `attention` is mapped to severity `warning`. The summary is cut to 1024 characters. A failed run triggers a second incident from the node's. Resolve is sent only for node instances and approvals this exporter alerted on (the list is kept in `open-alerts.json` and `approvals-pending.json`), so an incident opened before a state directory was lost is not resolved. Change events and links are not used.
- **DingTalk.** The signing recipe (key and message both `timestamp\nsecret`, standard base64, then URL-encoded) is from the robot security documentation. Keyword security relies on the word `Ogun` in the title; IP allow-listing is yours to configure. No `@` mentions.
- **Feishu and Lark.** Only plain text messages; interactive cards are not sent. The signing recipe differs from DingTalk's (the key is the string and the message is empty). Both `code` and `StatusCode` replies are accepted. The one-hour timestamp window depends on a correct clock.
- **WeCom.** The 4096-byte markdown limit is applied as 4000 bytes. Markdown support is a subset of the usual syntax. Per-bot rate limits are not modelled beyond `429` handling.
- **Splunk.** Events are concatenated without a separator; the snapshot is one event of sourcetype `ogun:snapshot` and not a HEC metric. Indexer acknowledgement is not used. `time` is a float in seconds.
- **Prometheus.** Pushgateway label values that need base64 encoding in the path (those containing `/`) are refused instead of encoded.
- **Elasticsearch.** `create` with an explicit `_id` is used so that data streams and plain indices both accept it, and `409` is treated as delivered. Index templates, mappings, lifecycle policies and the API key's privileges are not set up here.
- **OpenTelemetry.** 64-bit integers are JSON strings and keys are lower camel case, per the OTLP JSON mapping. Gzip and protobuf are not used. Collectors that demand protobuf will refuse the bodies.
- **New Relic.** `Api-Key` is used for both APIs, and attribute names are free-form. The 1 MB body limit is respected only by sending at most 200 items per request.
- **Loki.** Streams are grouped by `level` and `workflow`, in time order. Older-than-limit rejections are not special-cased.

Deliberately left out: gzip request bodies, a switch to turn certificate verification off, proxy settings (the standard proxy environment variables apply to non-loopback hosts), OAuth flows, Slack threads and Teams actions, DataHub/OpenMetadata/Atlan path presets, Datadog logs and traces, replaying history on first start, a lock against two exporters on one state directory, and a status command (counts of pending and dropped batches are printed by `run --once`).
