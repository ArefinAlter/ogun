# airflow-provider-ogun

Apache Airflow provider for Ogun (O-14 section 5, ADR 0012). Airflow calls Ogun: the provider starts runs of workflow versions already stored in `ogund`, waits for them or defers to the triggerer, and senses runs and partitions. It talks to the daemon's local API through [`ogun-client`](../python/ogun-client).

| Class | What it does |
|---|---|
| `ogun_airflow.hooks.ogun.OgunHook` | Connection type `ogun`; an authenticated `ogun_client.Client` |
| `ogun_airflow.operators.ogun.OgunRunOperator` | Start a run (latest, keys, or a backfill range), bound to `workflow_hash` when given; wait, defer, or return at once; push `ogun_run_id` and the artifact digests to XCom |
| `ogun_airflow.sensors.ogun.OgunRunSensor` | Succeeds when a run succeeds; fails when it fails or is cancelled |
| `ogun_airflow.sensors.ogun.OgunPartitionSensor` | Succeeds when one partition is `done` for the workflow's hash |
| `ogun_airflow.triggers.ogun.OgunRunTrigger` | The deferrable wait |

```sh
pip install ./integrations/python/ogun-client ./integrations/airflow
```

## An agent credential, and never an approval

The provider authenticates with a local credential of kind `agent`. A person creates it once, on a terminal of the machine that runs `ogund`, as the user the Airflow workers run as:

```sh
ogun credentials add airflow --kind agent
```

Then add an Airflow connection of type `ogun` named `ogun_default`. It holds no secret; its extras only say which credential file to read:

```json
{"credential": "airflow", "ogun_home": null, "profile": null}
```

The token stays in Ogun's state directory (`credentials/airflow.token`, mode `0600`). The client refuses non-agent credentials, so a DAG never borrows a person's authority.

**This provider can never approve anything.** An `agent` credential cannot call `approvals.decide` (O-15 section 6), and the client refuses to send it. If the workflow version needs an approval (new code, new egress, a budget over policy), the run waits until a person approves on the Ogun CLI or in Ogun Studio; the operator keeps waiting until then or until its `timeout`. Pin `workflow_hash` to the version a person reviewed, and the run cannot silently pick up a newer one.

## Idempotency

`OgunRunOperator` derives its idempotency key from the DAG id, task id, DAG run id and map index. An Airflow retry after a worker crash re-attaches to the run it already started; a retry after that run failed or was cancelled starts a fresh run with a key that includes the try number. Pass `idempotency_key` to choose your own.

## Example

[`example_dags/ogun_noaa_daily.py`](example_dags/ogun_noaa_daily.py): run each DAG run's day, sense it, backfill the last 30 days deferrably, and wait for the partition.

## Requirements

Airflow 2.8 or newer (Airflow 3's `airflow.sdk` imports are used when present), Python 3.9 or newer, and a running `ogund` reachable by the worker's user on the same machine (the local API has no network listener; for remote workers, use the server mode bridge when it exists, ADR 0010).
