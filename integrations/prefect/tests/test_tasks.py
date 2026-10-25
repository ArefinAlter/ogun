from contextlib import contextmanager
import logging

import pytest

pytest.importorskip("prefect")
from prefect_ogun import tasks


@pytest.mark.parametrize("state", ["succeeded", "failed"])
def test_run_uses_pin_closes_client_and_propagates_failure(monkeypatch, state):
    calls = []

    class FakeClient:
        def start_run(self, workflow, selector, **kwargs):
            calls.append((workflow, selector, kwargs))
            return {"run_id": "run", "state": "running"}

        def wait_for_run(self, run_id, **kwargs):
            return {"run_id": run_id, "state": state, "workflow_hash": "hash"}

        def list_artifacts(self, run_id):
            return [{"blake3": "digest"}]

    class Connection:
        @contextmanager
        def client(self):
            try:
                yield FakeClient()
            finally:
                calls.append("closed")

    monkeypatch.setattr(tasks, "get_run_logger", lambda: logging.getLogger("test"))
    kwargs = dict(partition_keys=["2026-01-02"], workflow_hash="hash", version=3, idempotency_key="retry-key", connection=Connection())
    if state == "failed":
        with pytest.raises(RuntimeError, match="ended failed"):
            tasks.ogun_run.fn("daily", **kwargs)
    else:
        result = tasks.ogun_run.fn("daily", **kwargs)
        assert result["artifacts"][0]["blake3"] == "digest"
    assert calls[0] == ("daily", {"kind": "keys", "keys": ["2026-01-02"]}, {"idempotency_key": "retry-key", "version": 3, "workflow_hash": "hash"})
    assert calls[-1] == "closed"
