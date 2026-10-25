"""``OgunRunTrigger``: waits for an Ogun run in the triggerer, so a deferred task holds no worker."""

from __future__ import annotations

import asyncio
import time
from typing import Any, AsyncIterator, Dict, Optional, Tuple

from .._compat import BaseTrigger, TriggerEvent


class OgunRunTrigger(BaseTrigger):
    """Polls ``runs.get`` until the run is terminal or ``timeout`` (seconds) passes."""

    def __init__(
        self,
        run_id: str,
        ogun_conn_id: Optional[str] = "ogun_default",
        poll_interval: float = 30.0,
        timeout: Optional[float] = None,
        started_at: Optional[float] = None,
    ) -> None:
        super().__init__()
        self.run_id = run_id
        self.ogun_conn_id = ogun_conn_id
        self.poll_interval = poll_interval
        self.timeout = timeout
        self.started_at = started_at if started_at is not None else time.time()

    def serialize(self) -> Tuple[str, Dict[str, Any]]:
        return (
            "ogun_airflow.triggers.ogun.OgunRunTrigger",
            {
                "run_id": self.run_id,
                "ogun_conn_id": self.ogun_conn_id,
                "poll_interval": self.poll_interval,
                "timeout": self.timeout,
                "started_at": self.started_at,
            },
        )

    def _poll(self) -> Dict[str, Any]:
        from ..hooks.ogun import OgunHook

        hook = OgunHook(self.ogun_conn_id)
        try:
            return hook.get_run(self.run_id)
        finally:
            hook.close()

    async def run(self) -> AsyncIterator[TriggerEvent]:  # type: ignore[override]
        from ogun_client import TERMINAL_RUN_STATES

        while True:
            try:
                run = await asyncio.to_thread(self._poll)
            except Exception as e:  # noqa: BLE001 - reported to the task, which fails
                yield TriggerEvent({"status": "error", "run_id": self.run_id, "message": str(e)})
                return
            state = run.get("state")
            if state in TERMINAL_RUN_STATES:
                yield TriggerEvent({"status": "done", "run_id": self.run_id, "state": state, "run": run})
                return
            if self.timeout is not None and time.time() - self.started_at > self.timeout:
                yield TriggerEvent({"status": "timeout", "run_id": self.run_id, "state": state})
                return
            await asyncio.sleep(self.poll_interval)
