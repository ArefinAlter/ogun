"""Prefect tasks that start and wait for Ogun runs.

Every task can be called inside a flow; the idempotency key defaults to one derived from the
flow run and the task run, so a Prefect retry re-attaches to the Ogun run it started. A run that
needs an approval waits until a person approves on the Ogun CLI or in Ogun Studio; no task can.
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional

from ogun_client import TERMINAL_RUN_STATES, key_range, keys, latest
from ogun_client import idempotency_key as make_key
from prefect import get_run_logger, task

from .blocks import OgunConnection

APPROVAL_NOTE = "If the run waits for an approval, a person approves it on the Ogun CLI or in Ogun Studio; Prefect cannot."


def _default_key(workflow: str) -> str:
    from prefect.runtime import flow_run, task_run

    flow_id, task_id = getattr(flow_run, "id", None), getattr(task_run, "id", None)
    if not flow_id:
        raise ValueError("outside a flow run, pass idempotency_key explicitly")
    return make_key("prefect", flow_id, task_id or "", workflow)


def selector(
    partitions: Optional[Dict[str, Any]] = None,
    partition_keys: Optional[List[str]] = None,
    partition_from: Optional[str] = None,
    partition_to: Optional[str] = None,
    force: bool = False,
) -> Dict[str, Any]:
    """A partition selector from the usual arguments; ``latest`` when none is given."""
    if partitions:
        return dict(partitions)
    if partition_keys:
        return keys([str(k) for k in partition_keys])
    if partition_from and partition_to:
        return key_range(partition_from, partition_to, force)
    return latest()


def _digests(artifacts: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    return [
        {k: a.get(k) for k in ("blake3", "node", "output", "partition", "size_bytes", "media_type")} for a in artifacts
    ]


@task(name="ogun-start-run")
def start_ogun_run(
    workflow: str,
    partitions: Optional[Dict[str, Any]] = None,
    *,
    partition_keys: Optional[List[str]] = None,
    partition_from: Optional[str] = None,
    partition_to: Optional[str] = None,
    force: bool = False,
    workflow_hash: Optional[str] = None,
    version: Optional[int] = None,
    idempotency_key: Optional[str] = None,
    connection: Optional[OgunConnection] = None,
) -> Dict[str, Any]:
    """Starts a run of a stored workflow version and returns its run view."""
    conn = connection or OgunConnection()
    key = idempotency_key or _default_key(workflow)
    with conn.client() as c:
        run = c.start_run(
            workflow,
            selector(partitions, partition_keys, partition_from, partition_to, force),
            idempotency_key=key,
            version=version,
            workflow_hash=workflow_hash,
        )
    get_run_logger().info("Ogun run %s of %s is %s", run.get("run_id"), workflow, run.get("state"))
    return run


@task(name="ogun-wait-for-run")
def wait_for_ogun_run(
    run_id: str,
    *,
    timeout_s: Optional[float] = None,
    poll_interval_s: float = 5.0,
    connection: Optional[OgunConnection] = None,
) -> Dict[str, Any]:
    """Waits until the run ends; returns ``{run_id, workflow_hash, state, artifacts}`` and raises
    ``RuntimeError`` when the run failed or was cancelled."""
    conn = connection or OgunConnection()
    log = get_run_logger()

    def update(run: Dict[str, Any]) -> None:
        log.info("Ogun run %s is %s", run.get("run_id"), run.get("state"))
        if run.get("state") == "waiting":
            log.info(APPROVAL_NOTE)

    with conn.client() as c:
        final = c.wait_for_run(run_id, timeout=timeout_s, poll_interval=poll_interval_s, on_update=update)
        artifacts = c.list_artifacts(run_id) if final.get("state") == "succeeded" else []
    if final.get("state") != "succeeded":
        raise RuntimeError(f"Ogun run {run_id} ended {final.get('state')}")
    return {
        "run_id": run_id,
        "workflow_hash": final.get("workflow_hash"),
        "state": final.get("state"),
        "artifacts": _digests(artifacts),
    }


@task(name="ogun-run")
def ogun_run(
    workflow: str,
    partitions: Optional[Dict[str, Any]] = None,
    *,
    partition_keys: Optional[List[str]] = None,
    partition_from: Optional[str] = None,
    partition_to: Optional[str] = None,
    force: bool = False,
    workflow_hash: Optional[str] = None,
    version: Optional[int] = None,
    idempotency_key: Optional[str] = None,
    timeout_s: Optional[float] = None,
    poll_interval_s: float = 5.0,
    connection: Optional[OgunConnection] = None,
) -> Dict[str, Any]:
    """Starts a run and waits for it: ``{run_id, workflow_hash, state, artifacts}``."""
    conn = connection or OgunConnection()
    log = get_run_logger()
    key = idempotency_key or _default_key(workflow)
    with conn.client() as c:
        run = c.start_run(
            workflow,
            selector(partitions, partition_keys, partition_from, partition_to, force),
            idempotency_key=key,
            version=version,
            workflow_hash=workflow_hash,
        )
        log.info("Ogun run %s of %s is %s", run.get("run_id"), workflow, run.get("state"))

        def update(r: Dict[str, Any]) -> None:
            log.info("Ogun run %s is %s", r.get("run_id"), r.get("state"))
            if r.get("state") == "waiting":
                log.info(APPROVAL_NOTE)

        final = run if run.get("state") in TERMINAL_RUN_STATES else c.wait_for_run(
            run["run_id"], timeout=timeout_s, poll_interval=poll_interval_s, on_update=update
        )
        artifacts = c.list_artifacts(run["run_id"]) if final.get("state") == "succeeded" else []
    if final.get("state") != "succeeded":
        raise RuntimeError(f"Ogun run {run['run_id']} ended {final.get('state')}")
    return {
        "run_id": run["run_id"],
        "workflow_hash": final.get("workflow_hash"),
        "state": final.get("state"),
        "artifacts": _digests(artifacts),
    }


@task(name="ogun-partition-state")
def ogun_partition_state(
    workflow: str,
    partition_key: str,
    *,
    version: Optional[int] = None,
    connection: Optional[OgunConnection] = None,
) -> Optional[str]:
    """The state of one partition: ``done``, ``running``, ``failed``, ``missing`` or ``None``."""
    conn = connection or OgunConnection()
    with conn.client() as c:
        return c.partition_state(workflow, partition_key, version)
