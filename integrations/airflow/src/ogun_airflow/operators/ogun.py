"""``OgunRunOperator``: start a run of a stored, approved workflow version and wait for it."""

from __future__ import annotations

import time
from typing import Any, Dict, List, Optional, Sequence

from ogun_client import idempotency_key as make_key
from ogun_client import key_range, keys, latest

from .._compat import AirflowException, BaseOperator
from ..hooks.ogun import OgunHook
from ..triggers.ogun import OgunRunTrigger

WAITING_NOTE = (
    "The Ogun run is waiting, possibly for an approval. A person approves on the Ogun CLI or in "
    "Ogun Studio; Airflow cannot."
)


def _artifact_digests(hook: OgunHook, run_id: str) -> List[Dict[str, Any]]:
    try:
        artifacts = hook.list_artifacts(run_id)
    except AirflowException:
        return []
    return [
        {
            "blake3": a.get("blake3"),
            "node": a.get("node"),
            "output": a.get("output"),
            "partition": a.get("partition"),
            "size_bytes": a.get("size_bytes"),
            "media_type": a.get("media_type"),
        }
        for a in artifacts
    ]


class OgunRunOperator(BaseOperator):
    """Starts an Ogun run and, by default, waits until it ends.

    The run binds to ``workflow_hash`` when given, so the task runs exactly the version a person
    reviewed and approved. The idempotency key defaults to one derived from the DAG run, task and
    map index, so an Airflow retry re-attaches to the run it started instead of starting another;
    a retry after that run failed or was cancelled starts a fresh one.

    Partitions: ``partitions`` (a selector dict), or ``partition_keys``, or ``partition_from`` and
    ``partition_to`` (a backfill range; ``force`` re-runs keys already done). All are templated,
    so ``partition_keys=["{{ ds }}"]`` runs the DAG run's day.

    The return value (and XCom ``return_value``) is ``{run_id, workflow, workflow_hash, state,
    artifacts: [{blake3, node, output, partition, size_bytes, media_type}]}``; ``run_id`` is also
    pushed under the XCom key ``ogun_run_id`` as soon as the run starts.

    The operator can never approve. A run that needs an approval waits until a person decides on
    the Ogun CLI or in Studio, or until ``timeout`` passes.
    """

    template_fields: Sequence[str] = (
        "workflow",
        "workflow_hash",
        "partitions",
        "partition_keys",
        "partition_from",
        "partition_to",
        "idempotency_key",
    )
    ui_color = "#d9e6f2"

    def __init__(
        self,
        *,
        workflow: str,
        workflow_hash: Optional[str] = None,
        version: Optional[int] = None,
        partitions: Optional[Dict[str, Any]] = None,
        partition_keys: Optional[List[str]] = None,
        partition_from: Optional[str] = None,
        partition_to: Optional[str] = None,
        force: bool = False,
        idempotency_key: Optional[str] = None,
        wait_for_completion: bool = True,
        deferrable: bool = False,
        poll_interval: float = 30.0,
        timeout: Optional[float] = None,
        cancel_on_kill: bool = True,
        ogun_conn_id: str = OgunHook.default_conn_name,
        **kwargs: Any,
    ) -> None:
        super().__init__(**kwargs)
        self.workflow = workflow
        self.workflow_hash = workflow_hash
        self.version = version
        self.partitions = partitions
        self.partition_keys = partition_keys
        self.partition_from = partition_from
        self.partition_to = partition_to
        self.force = force
        self.idempotency_key = idempotency_key
        self.wait_for_completion = wait_for_completion
        self.deferrable = deferrable
        self.poll_interval = poll_interval
        self.ogun_timeout = timeout
        self.cancel_on_kill = cancel_on_kill
        self.ogun_conn_id = ogun_conn_id
        self.run_id: Optional[str] = None

    def _selector(self) -> Dict[str, Any]:
        if self.partitions:
            return dict(self.partitions)
        if self.partition_keys:
            return keys([str(k) for k in self.partition_keys])
        if self.partition_from and self.partition_to:
            return key_range(str(self.partition_from), str(self.partition_to), self.force)
        return latest()

    def _key(self, context: Dict[str, Any]) -> str:
        if self.idempotency_key:
            return make_key(self.idempotency_key)
        ti = context.get("ti") or context.get("task_instance")
        run_id = context.get("run_id") or getattr(ti, "run_id", "")
        map_index = getattr(ti, "map_index", -1)
        return make_key("airflow", self.dag_id, self.task_id, run_id, map_index)

    def _result(self, hook: OgunHook, run: Dict[str, Any]) -> Dict[str, Any]:
        return {
            "run_id": run.get("run_id"),
            "workflow": run.get("workflow", self.workflow),
            "workflow_hash": run.get("workflow_hash"),
            "state": run.get("state"),
            "artifacts": _artifact_digests(hook, str(run.get("run_id"))),
        }

    def _finish(self, hook: OgunHook, run: Dict[str, Any]) -> Dict[str, Any]:
        state = run.get("state")
        if state != "succeeded":
            raise AirflowException(f"Ogun run {run.get('run_id')} ended {state}")
        return self._result(hook, run)

    def execute(self, context: Dict[str, Any]) -> Any:
        hook = OgunHook(self.ogun_conn_id)
        try:
            key = self._key(context)
            selector = self._selector()
            run = hook.start_run(self.workflow, selector, key, self.version, self.workflow_hash)
            ti = context.get("ti")
            try_number = int(getattr(ti, "try_number", 1) or 1)
            if run.get("state") in ("failed", "cancelled") and try_number > 1:
                # An earlier try's run ended badly; this retry wants a new run, not the old result.
                run = hook.start_run(
                    self.workflow, selector, make_key(key, "try", try_number), self.version, self.workflow_hash
                )
            self.run_id = str(run["run_id"])
            if ti is not None:
                ti.xcom_push(key="ogun_run_id", value=self.run_id)
            self.log.info("Ogun run %s of %s is %s", self.run_id, self.workflow, run.get("state"))
            if not self.wait_for_completion:
                return self._result(hook, run)
            if hook.is_terminal(run):
                return self._finish(hook, run)
            if self.deferrable:
                self.defer(
                    trigger=OgunRunTrigger(
                        run_id=self.run_id,
                        ogun_conn_id=self.ogun_conn_id,
                        poll_interval=self.poll_interval,
                        timeout=self.ogun_timeout,
                    ),
                    method_name="execute_complete",
                )
            return self._finish(hook, self._wait(hook, run))
        finally:
            hook.close()

    def _wait(self, hook: OgunHook, run: Dict[str, Any]) -> Dict[str, Any]:
        deadline = None if self.ogun_timeout is None else time.monotonic() + self.ogun_timeout
        noted = False
        while not hook.is_terminal(run):
            if run.get("state") == "waiting" and not noted:
                self.log.info(WAITING_NOTE)
                noted = True
            if deadline is not None and time.monotonic() > deadline:
                raise AirflowException(f"Ogun run {run.get('run_id')} is still {run.get('state')} after the timeout")
            time.sleep(self.poll_interval)
            run = hook.get_run(str(run["run_id"]))
        return run

    def execute_complete(self, context: Dict[str, Any], event: Dict[str, Any]) -> Any:
        """Resumes after the trigger fires."""
        status = event.get("status")
        if status == "timeout":
            raise AirflowException(f"Ogun run {event.get('run_id')} is still {event.get('state')} after the timeout")
        if status != "done":
            raise AirflowException(f"waiting for Ogun run {event.get('run_id')} failed: {event.get('message')}")
        hook = OgunHook(self.ogun_conn_id)
        try:
            return self._finish(hook, event.get("run") or {"run_id": event.get("run_id"), "state": event.get("state")})
        finally:
            hook.close()

    def on_kill(self) -> None:
        if not (self.cancel_on_kill and self.run_id):
            return
        hook = OgunHook(self.ogun_conn_id)
        try:
            hook.cancel_run(self.run_id, make_key("airflow-kill", self.run_id))
            self.log.info("Cancelled Ogun run %s", self.run_id)
        except AirflowException as e:
            self.log.warning("Could not cancel Ogun run %s: %s", self.run_id, e)
        finally:
            hook.close()
