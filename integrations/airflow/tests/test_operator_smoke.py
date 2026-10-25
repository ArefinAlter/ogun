"""Local operator smoke with a minimal Airflow API stand-in, without an Airflow installation."""
import importlib
import logging
import sys
from types import ModuleType, SimpleNamespace

import pytest


@pytest.mark.parametrize("state", ["succeeded", "failed"])
def test_operator_forwards_pin_records_run_and_propagates_failure(monkeypatch, state):
    compat = ModuleType("ogun_airflow._compat")

    class AirflowException(Exception):
        pass

    class Base:
        def __init__(self, task_id="task", **kwargs):
            self.task_id = task_id
            self.dag_id = "dag"
            self.log = logging.getLogger("test")

    compat.AirflowException = AirflowException
    compat.BaseHook = compat.BaseOperator = compat.BaseSensorOperator = compat.BaseTrigger = Base
    compat.TriggerEvent = dict
    monkeypatch.setitem(sys.modules, "ogun_airflow._compat", compat)
    names = ["ogun_airflow.hooks.ogun", "ogun_airflow.triggers.ogun", "ogun_airflow.operators.ogun"]
    for name in names:
        monkeypatch.delitem(sys.modules, name, raising=False)
    operator = importlib.import_module("ogun_airflow.operators.ogun")
    calls = []

    class Hook:
        def __init__(self, conn_id):
            pass

        def start_run(self, *args):
            calls.append(args)
            return {"run_id": "run", "state": state, "workflow_hash": "hash"}

        def is_terminal(self, run):
            return True

        def list_artifacts(self, run_id):
            return [{"blake3": "digest"}]

        def close(self):
            calls.append("closed")

    monkeypatch.setattr(operator, "OgunHook", Hook)
    pushes = []
    ti = SimpleNamespace(try_number=1, map_index=-1, xcom_push=lambda **kwargs: pushes.append(kwargs))
    op = operator.OgunRunOperator(workflow="daily", workflow_hash="hash", version=3, partition_keys=["2026-01-02"], idempotency_key="retry-key")
    try:
        if state == "failed":
            with pytest.raises(AirflowException, match="ended failed"):
                op.execute({"ti": ti})
        else:
            assert op.execute({"ti": ti})["artifacts"][0]["blake3"] == "digest"
        assert calls[0] == ("daily", {"kind": "keys", "keys": ["2026-01-02"]}, "retry-key", 3, "hash")
        assert calls[-1] == "closed"
        assert pushes == [{"key": "ogun_run_id", "value": "run"}]
    finally:
        for name in names:
            sys.modules.pop(name, None)
