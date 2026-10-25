# Bike-share example workflows

Six workflows on a bike-share network, used by Studio's scripted build (`apps/studio/web/src/demo`) and by the tests that
check every example against the validator and every code hash against its file. The data they name is invented, except
the public weather API.

| Workflow | What it shows |
|---|---|
| [`dock-telemetry`](dock-telemetry.workflow.json) | Bike and dock counts from stations over MQTT, sealed into one-minute windows that survive outages and rolled up hourly. Late data from a station that was offline becomes a new revision, not a silent gap. |
| [`trips-backfill`](trips-backfill.workflow.json) | Every station, month by month since 2019, from a public trip archive. A partitioned backfill: a crash or a dropped link resumes only the unfinished months. |
| [`rebalance-plan`](rebalance-plan.workflow.json) | A daily empty-or-full risk per station and the shortlist to rebalance. |
| [`rebalance-plan` v2](rebalance-plan.v2.workflow.json) | The same with a weather adjustment. Changing the model changes its code hash, so the new version needs a person's approval before it runs (`ogun diff` says so). |
| [`district-report`](district-report.workflow.json) | A monthly service report per district, in English and Spanish. Nothing is sent until a person approves it. |
| [`ridership-forecast`](ridership-forecast.workflow.json) | Next week's trips per station from the same week last year, scaled by the weather forecast. |

`ops/` holds the SQL and Python the workflows run. Each workflow names its script by BLAKE3 digest, and a test checks
that every digest matches its file. The models are deliberately simple.

```sh
cargo build -p ogun-cli
ogun validate examples/bikeshare/rebalance-plan.workflow.json
ogun diff examples/bikeshare/rebalance-plan.workflow.json examples/bikeshare/rebalance-plan.v2.workflow.json
ogun partitions examples/bikeshare/trips-backfill.workflow.json
ogun next examples/bikeshare/district-report.workflow.json
```
