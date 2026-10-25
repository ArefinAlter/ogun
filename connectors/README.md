# Connectors

A connector is a manifest that describes one external system Ogun can read from, write to, send telemetry to, or be
called by: what it is, how it authenticates (names only), where it connects, and which operators it would
contribute. The contract is [O-16](../docs/spec/O-16-connectors-and-connections.md) and the decision is
[ADR 0013](../docs/adr/0013-connectors.md) (Proposed).

| Path | What |
|---|---|
| `schema/connector.v0.schema.json` | The manifest schema. |
| `catalog/*.connector.json` | One manifest per system. 99 today. |
| `tools/check.mjs` | The validator: the schema, plus the rules a schema cannot say (secret fields, domain placeholders, unique operators, code that exists). Dependency-free. |
| `tools/check.test.mjs` | Shows each rule failing. |

```sh
node connectors/tools/check.mjs
node --test connectors/tools/check.test.mjs
```

## What is in the catalog

| Category | Examples |
|---|---|
| Warehouses and lakehouses | BigQuery, Snowflake, Databricks, Redshift, Athena, Azure Synapse, Microsoft Fabric, ClickHouse, MotherDuck, Trino; Alibaba MaxCompute, Hologres and AnalyticDB, Tencent DLC, Huawei DLI and GaussDB(DWS), Volcano Engine ByteHouse, StarRocks, Apache Doris |
| Databases | PostgreSQL, MySQL, SQL Server, Oracle, MongoDB, TiDB, DuckDB |
| Object storage | S3, Google Cloud Storage, Azure Blob, Cloudflare R2, MinIO, Backblaze B2, any S3-compatible store, SFTP; Alibaba OSS, Tencent COS, Huawei OBS, Baidu BOS, Volcano Engine TOS |
| Streaming | Kafka (and Redpanda, MSK, Confluent), Kinesis, Pub/Sub, Event Hubs, Pulsar, Alibaba SLS |
| Orchestrators (they call Ogun) | Airflow, Dagster, Prefect, Kestra, dbt, Temporal, Argo, GitHub Actions, Azure Data Factory, AWS Glue, DolphinScheduler, Alibaba DataWorks |
| Observability | Datadog, Grafana and Loki, Prometheus, OpenTelemetry, New Relic, Splunk, Elastic, Sentry, CloudWatch, Azure Monitor |
| Lineage and catalogs | OpenLineage, Marquez, DataHub, OpenMetadata, Atlan, Glue Data Catalog, Unity Catalog, Iceberg REST |
| Notifications | Slack, Teams, PagerDuty, email, a signed webhook, DingTalk, Feishu and Lark, WeCom |
| Public data | OpenAQ, Open-Meteo, NOAA, Hugging Face, Kaggle, GitHub |
| Edge and agents | MQTT, HTTP push, LoRaWAN, Modbus, file tail; MCP, VS Code, the language server |

## What "built" and "manifest only" mean

Read this before you promise anything to anyone.

- **Manifest only** (`contract`): the manifest says what Ogun would connect to. No operator exists and nothing has
  been run against that system. This is most of the catalog.
- **Built**: the code exists and passes its own tests. It has not run against the real service or device. Today
  that is the orchestrator packages, the edge ingest sources, the editor and agent surfaces, and the sinks of
  `integrations/export` (checked against local fakes written from public documentation).
- **Verified**: run end to end against the real service, with the evidence cited. Nothing is verified yet.

Vendor facts were written from public documentation by several authors. Each manifest's `notes` says what its author
was unsure of. Treat a domain, a port or a region as a claim to check, not as a fact, until the connector is
`verified`.

## Adding a connector

1. Copy the closest manifest in `catalog/` and change it. Use `contract` unless you are adding code.
2. `node connectors/tools/check.mjs` until it is clean.
3. `cd apps/studio/web && node scripts/sync-connectors.mjs` to refresh Studio's bundled copy.
4. Say in `notes` anything you were not sure of.
