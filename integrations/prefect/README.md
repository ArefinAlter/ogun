# prefect-ogun

Prefect integration for Ogun (O-14 section 5, ADR 0012). Prefect calls Ogun through [`ogun-client`](../python/ogun-client):

| Name | Kind | What it does |
|---|---|---|
| `OgunConnection` | Block | How to reach `ogund`: credential name or file, `ogun_home`, `profile`. Holds no secret. |
| `ogun_run` | Task | Start a run and wait: `{run_id, workflow_hash, state, artifacts}` with BLAKE3 digests |
| `start_ogun_run` | Task | Start a run; returns the run view |
| `wait_for_ogun_run` | Task | Wait for a run; fails when it fails or is cancelled |
| `ogun_partition_state` | Task | `done`, `running`, `failed`, `missing` for one partition key |

```python
from prefect import flow
from prefect_ogun import OgunConnection, ogun_run

@flow
def nightly(day: str):
    return ogun_run(
        "noaa-daily",
        partition_keys=[day],
        workflow_hash="…",                      # the version a person reviewed and approved
        connection=OgunConnection(credential="prefect"),
    )
```

```sh
pip install ./integrations/python/ogun-client ./integrations/prefect
prefect block register -m prefect_ogun
```

## An agent credential, and never an approval

A person creates the credential once, on a terminal of the machine that runs `ogund`, as the user the Prefect worker runs as:

```sh
ogun credentials add prefect --kind agent
```

The block names the credential file; the token stays in Ogun's state directory (`credentials/prefect.token`, `0600`), so nothing secret is stored in Prefect. The client accepts only `agent` credentials, and an agent can never decide an approval (O-15 section 6). A run of a version that needs one waits until a person approves on the Ogun CLI or in Ogun Studio.

## Idempotency

Inside a flow, the key defaults to one derived from the flow run id, the task run id and the workflow, so a Prefect retry re-attaches to the Ogun run it started. Outside a flow, pass `idempotency_key`.

Requirements: Prefect 2.14 or newer, Python 3.9 or newer, and `ogund` reachable on the same machine as the worker.
