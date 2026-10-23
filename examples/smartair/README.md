# Smart Air Bangladesh: example workflows

Example workflows for an informal conversation with Smart Air Bangladesh about their air-quality data. Each one maps to a line of their business, and each passes the Ogun validator today.

Ogun is pre-alpha. These files show what the platform would run and what a person would approve. The runtime that executes them (`ogund`, the ingest bridge and the operators) is being built. Present every item as a co-development pilot, not as a product.

| Workflow | For | What it shows |
|---|---|---|
| [`../air-quality-stream.workflow.json`](../air-quality-stream.workflow.json) | The "Air Quality Insight" dashboards for offices and schools | Indoor and outdoor monitors over MQTT, sealed into one-minute windows that survive outages, rolled up hourly. Late data from a device that was offline becomes a new revision, not a silent gap. |
| [`filter-life.workflow.json`](filter-life.workflow.json) | Filter replacement and a subscription offer | A daily filter-load estimate per purifier, following Smart Air's own 1,400-hour method, and the list of filters due within three weeks. |
| [`filter-life.v2.workflow.json`](filter-life.v2.workflow.json) | The same, after calibrating with Dhaka bench tests | Changing the model changes its code hash, so the new version needs a person's approval before it runs. |
| [`openaq-bangladesh.workflow.json`](openaq-bangladesh.workflow.json) | Public reference data next to their own sensors | Every OpenAQ monitor in Bangladesh, month by month since 2019, from the public archive. A crash or a dropped link resumes only the unfinished months. |
| [`school-report.workflow.json`](school-report.workflow.json) | The 5 Dhaka schools and their parents | A monthly report card per school, in English and Bangla: indoor against outdoor PM2.5, and hours above the national limit and the WHO guideline. Nothing is sent until a person approves it. |
| [`demand-forecast.workflow.json`](demand-forecast.workflow.json) | Stock and campaign timing before the November-to-March peak | Next week's demand per SKU, from last year's sales scaled by the CAMS PM2.5 forecast. |

`ops/` holds the SQL and Python the workflows run. Each workflow names its script by BLAKE3 digest, and a test checks that every digest matches its file. The models are deliberately simple and need Smart Air's own test data before any customer sees a number.

## Five-minute demo

```sh
cargo build -p ogun-cli

# 1. A workflow is data: it is validated before anything runs, and has one hash.
ogun validate examples/smartair/filter-life.workflow.json

# 2. Change the model, and Ogun says exactly what a person must approve.
ogun diff examples/smartair/filter-life.workflow.json examples/smartair/filter-life.v2.workflow.json

# 3. Seven years of public data as resumable monthly partitions.
ogun partitions examples/smartair/openaq-bangladesh.workflow.json

# 4. The continuous sensor workflow, and when the scheduled ones next run.
ogun validate examples/air-quality-stream.workflow.json
ogun next examples/smartair/school-report.workflow.json

# 5. A mistake the validator catches: a password typed into the workflow.
sed 's/{ "secret_ref": "site_a_mqtt" }/"hunter2"/' examples/air-quality-stream.workflow.json > /tmp/bad.workflow.json
ogun validate /tmp/bad.workflow.json
```

The same checks run inside VS Code (`extensions/vscode`) and are available to coding agents through `ogun mcp`.

## Data-source terms to respect

- **OpenAQ:** attribution required, and use must complement rather than replicate OpenAQ.
- **WAQI/aqicn:** forbids paid use and redistribution without an agreement, so it is not used here.
- **BMD (the Bangladesh Meteorological Department):** data may be used only for the stated purpose, so it cannot be redistributed.
- **Open-Meteo:** commercial use needs a key and CAMS attribution.

Check each licence again before a pilot.
