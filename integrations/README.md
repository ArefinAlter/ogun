# Integrations

Ways for coding agents and data platforms to reach Ogun (O-14). Every one of them reaches `ogund` with a local credential of kind `agent`, which a person creates once on a terminal with `ogun credentials add <name> --kind agent`. None of them can decide an approval: a run that needs one waits until a person approves on the Ogun CLI or in Ogun Studio.

| Directory | What | Credential |
|---|---|---|
| [`mcp/`](mcp) | Configurations of `ogun mcp` for Claude Code, Claude Desktop, Cursor, VS Code, Windsurf, Zed and Codex CLI | `mcp` |
| [`python/ogun-client/`](python/ogun-client) | The O-15 client in Python: endpoint discovery, handshake, NDJSON JSON-RPC, typed helpers | named by the caller |
| [`airflow/`](airflow) | `airflow-provider-ogun`: `OgunHook`, `OgunRunOperator`, `OgunRunSensor`, `OgunPartitionSensor` | `airflow` |
| [`dagster/`](dagster) | `dagster-ogun`: `OgunResource` and `ogun_workflow_asset` | `dagster` |
| [`prefect/`](prefect) | `prefect-ogun`: the `OgunConnection` block and run tasks | `prefect` |
| [`kestra/`](kestra) | A flow blueprint with the `ogun` CLI and `ogun-client` | `kestra` |
| [`export/`](export) | `ogun-export`: follows the event stream and sends run, partition and approval information out to Datadog, OpenLineage, Slack, Teams, PagerDuty, DingTalk, Feishu, WeCom, Splunk, Prometheus, Elasticsearch, OpenTelemetry, New Relic, Loki and a signed webhook; read-only, with a durable outbox | `export` |

The packages are not on PyPI yet; install them from this directory (`pip install ./integrations/python/ogun-client ./integrations/airflow`).
