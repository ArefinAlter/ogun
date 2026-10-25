# dagster-ogun

Dagster integration for Ogun (O-14 section 5, ADR 0012). Dagster calls Ogun; Ogun stays the runtime that stores, schedules and verifies the work.

- **`OgunResource`**: connection settings for `ogund`'s local API, through [`ogun-client`](../python/ogun-client). `client()` opens an authenticated connection; `run_and_wait()` starts a run and waits for it.
- **`ogun_workflow_asset(workflow, ir=...)`**: one asset per Ogun workflow, partitioned like the workflow (`partitions_def_for`), materialised by starting Ogun runs.

| Ogun `partitions` (O-02 section 4.2) | Dagster partitions | Keys |
|---|---|---|
| `time`, `day` | `DailyPartitionsDefinition` | `YYYY-MM-DD` |
| `time`, `month` | `MonthlyPartitionsDefinition` | `YYYY-MM` |
| `static` | `StaticPartitionsDefinition` | the same keys |
| `window` (streams, O-09) | `TimeWindowPartitionsDefinition`, given `window_start` | `wYYYYMMDDTHHMMSSZ` |
| none | unpartitioned | |

A partition materialises as an Ogun run of exactly that key. A time-partition backfill runs as a single Dagster run (`BackfillPolicy.single_run()`) and one Ogun run over the range, which skips keys already done for the workflow hash. Window partitions are sealed by the stream, so materialising one checks that the window is sealed and fails while it is open. Each materialisation records the Ogun run id, the workflow hash and the BLAKE3 digest of every artifact.

```python
from dagster import Definitions
from dagster_ogun import OgunResource, ogun_workflow_asset

noaa = ogun_workflow_asset(
    "noaa-daily",
    ir="workflows/noaa-daily.workflow.json",
    workflow_hash="…",            # the version a person reviewed and approved
)

defs = Definitions(assets=[noaa], resources={"ogun": OgunResource(credential="dagster")})
```

```sh
pip install ./integrations/python/ogun-client ./integrations/dagster
```

## An agent credential, and never an approval

A person creates the credential once, on a terminal of the machine that runs `ogund`, as the user Dagster runs as:

```sh
ogun credentials add dagster --kind agent
```

The resource only names the credential file; the token stays in Ogun's state directory (`credentials/dagster.token`, `0600`). The client accepts only `agent` credentials, and an agent can never decide an approval (O-15 section 6). If a workflow version needs one, its run waits until a person approves on the Ogun CLI or in Ogun Studio; the step keeps waiting until then or until `wait_timeout_s`.

## Idempotency

The key is derived from the Dagster run id, the asset key and the partitions, so a retried step re-attaches to the Ogun run it started.

## Not yet

O-14 names Dagster Pipes for reporting Ogun artifacts from inside Ogun runs. This first version reports them from the asset after the run instead; Pipes waits for the operator ABI (O-03) to carry a Pipes context.

Requirements: Dagster 1.7 or newer, Python 3.9 or newer, and `ogund` reachable on the same machine.
