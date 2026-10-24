# ADR 0012: Data platforms call Ogun; data and lineage flow out

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-23 |
| Refines | Proposed ADR 0004 (integrate, do not replace) |
| Research | `docs/research/OR5-edge-streams-platforms.md` section 4.3 |

## Context

Teams already run Airflow, Dagster, Prefect, Kestra, dbt, warehouses and Grafana. Ogun should slot into those stacks, not ask teams to move. ADR 0004 says so in principle. This record fixes the shape of each integration.

## Decision

1. **Orchestrators call Ogun.** Each integration is a thin wrapper over the stable `ogun ... --json` CLI contract, or the server's HTTP bridge:
   - an Airflow provider (`OgunRunOperator`, `OgunSensor`);
   - a Dagster resource through Dagster Pipes;
   - a Prefect task and block;
   - a Kestra plugin.

   They start runs of workflows that are already approved, and read status and artifacts. They never approve.
2. **dbt is an operator, not an adapter.** The `dbt` code operator runs `dbt build` in a sandbox and records run results as artifacts. Ogun does not pretend to be a warehouse.
3. **Warehouses are loaded from Parquet.** Ogun stages Parquet in object storage and runs the platform's native load: Snowflake `COPY INTO`, BigQuery load jobs, Databricks `COPY INTO`, Postgres `COPY`. Loads are idempotent by partition and revision.
4. **Lineage goes out as OpenLineage events**, with Ogun facets for artifacts and windows, delivered through an outbox to Marquez, DataHub or any OpenLineage endpoint.
5. **Grafana** gets a read-only query bridge over sealed windows and rollups first; a Grafana Live plugin for streams comes later.

## Consequences

- There is maintenance breadth: Python, Java and Go ecosystems. Each wrapper stays small because the CLI JSON contract carries the logic. That contract (O-14 section 4) becomes a compatibility promise.
- Ogun never has to be "the orchestrator", which keeps it out of a crowded and consolidating market (VISION section 14).

## Alternatives considered

- **Ogun drives the other platforms:** rejected. Teams already have their orchestrator.
- **A dbt adapter:** rejected. Ogun is not a query engine.
