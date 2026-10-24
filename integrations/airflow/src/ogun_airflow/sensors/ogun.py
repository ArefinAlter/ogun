"""Sensors: an Ogun run is finished; an Ogun partition is done."""

from __future__ import annotations

from typing import Any, Dict, Optional, Sequence

from .._compat import AirflowException, BaseSensorOperator
from ..hooks.ogun import OgunHook


class OgunRunSensor(BaseSensorOperator):
    """Waits until an Ogun run succeeds; fails the task when it fails or is cancelled.

    ``run_id`` is templated, for example ``"{{ ti.xcom_pull(task_ids='start', key='ogun_run_id') }}"``.
    A run waiting for an approval keeps the sensor waiting: only a person can approve it, on the
    Ogun CLI or in Studio.
    """

    template_fields: Sequence[str] = ("run_id",)

    def __init__(self, *, run_id: str, ogun_conn_id: str = OgunHook.default_conn_name, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.run_id = run_id
        self.ogun_conn_id = ogun_conn_id

    def poke(self, context: Dict[str, Any]) -> bool:
        hook = OgunHook(self.ogun_conn_id)
        try:
            run = hook.get_run(self.run_id)
        finally:
            hook.close()
        state = run.get("state")
        self.log.info("Ogun run %s is %s", self.run_id, state)
        if state in ("failed", "cancelled"):
            raise AirflowException(f"Ogun run {self.run_id} ended {state}")
        return state == "succeeded"


class OgunPartitionSensor(BaseSensorOperator):
    """Waits until one partition of a stored workflow is ``done`` for its current hash.

    ``partition_key`` is templated (``"{{ ds }}"`` for a daily workflow, a window key
    ``wYYYYMMDDTHHMMSSZ`` for a stream workflow). With ``fail_on_failed`` the task fails when
    the partition's last attempt failed instead of waiting for a retry.
    """

    template_fields: Sequence[str] = ("workflow", "partition_key")

    def __init__(
        self,
        *,
        workflow: str,
        partition_key: str,
        version: Optional[int] = None,
        fail_on_failed: bool = False,
        ogun_conn_id: str = OgunHook.default_conn_name,
        **kwargs: Any,
    ) -> None:
        super().__init__(**kwargs)
        self.workflow = workflow
        self.partition_key = partition_key
        self.version = version
        self.fail_on_failed = fail_on_failed
        self.ogun_conn_id = ogun_conn_id

    def poke(self, context: Dict[str, Any]) -> bool:
        hook = OgunHook(self.ogun_conn_id)
        try:
            state = hook.partition_state(self.workflow, str(self.partition_key), self.version)
        finally:
            hook.close()
        self.log.info("Ogun %s partition %s is %s", self.workflow, self.partition_key, state)
        if state == "failed" and self.fail_on_failed:
            raise AirflowException(f"Ogun {self.workflow} partition {self.partition_key} failed")
        return state == "done"
