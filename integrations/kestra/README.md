# Ogun for Kestra

A flow blueprint, [`flows/ogun-run.yaml`](flows/ogun-run.yaml), that starts a run of a stored, approved Ogun workflow version, waits for it, and exposes the run id, state and artifact digests as task outputs (O-14 section 5, ADR 0012). Kestra calls Ogun; Ogun stays the runtime.

| Task | Type | What it does |
|---|---|---|
| `preflight` | `io.kestra.plugin.scripts.shell.Commands` | With the `ogun` CLI JSON contract (`ogun validate --json`, `ogun hash --json`): when the namespace has `workflows/<name>.workflow.json`, it must be valid and hash to the pinned `workflow_hash` |
| `run` | `io.kestra.plugin.scripts.python.Script` | With [`ogun-client`](../python/ogun-client): start the run bound to the pinned hash, wait, emit outputs |

A `Schedule` trigger runs yesterday's partition every morning.

## Setup

On the Kestra worker host, which is the host where `ogund` runs:

```sh
pip install ./integrations/python/ogun-client    # for the worker's Python
ogun credentials add kestra --kind agent         # once, by a person on a terminal
```

Both tasks use the `Process` task runner. `ogund`'s only listener is a local socket owned by its user (O-15 section 2), so the tasks must run on that host, as that user. A Docker task runner cannot reach it unless the container runs as the same uid with the socket and the state directory mounted; the Process runner is the supported setup.

## An agent credential, and never an approval

The flow authenticates with the `kestra` credential of kind `agent`. The token stays in Ogun's state directory; nothing secret is stored in Kestra. An agent credential cannot decide an approval (O-15 section 6), and `ogun-client` refuses to send one. If the version needs an approval, the run waits until a person approves on the Ogun CLI or in Ogun Studio, and the task waits with it. Pin `workflow_hash` to the version a person reviewed.

## Idempotency

The key is `kestra:<namespace>:<flow>:<execution>:<task>`, the same on every attempt of the task, so a Kestra retry re-attaches to the Ogun run it started.

## Not yet

A Java plugin task (`io.verne.ogun.Run`, named in O-14) is not built. The blueprint covers the same behaviour with the script tasks; the plugin would remove the Python dependency on the worker.
