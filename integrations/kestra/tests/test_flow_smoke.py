"""Execute the embedded Python script with local fake Ogun methods; no Kestra service."""
import json
from pathlib import Path

import pytest
import ogun_client

yaml = pytest.importorskip("yaml")


@pytest.mark.parametrize("state", ["succeeded", "failed"])
def test_embedded_script_outputs_artifacts_and_reports_failure(monkeypatch, capsys, state):
    calls = []

    class Client:
        @classmethod
        def connect(cls, credential):
            assert credential == "kestra"
            return cls()

        def __enter__(self):
            return self

        def __exit__(self, *args):
            calls.append("closed")

        def start_run(self, workflow, selector, **kwargs):
            calls.append((workflow, selector, kwargs))
            return {"run_id": "run", "state": "running"}

        def wait_for_run(self, run_id, on_update):
            return {"run_id": run_id, "state": state, "workflow_hash": "hash"}

        def list_artifacts(self, run_id):
            return [{"blake3": "digest"}]

    monkeypatch.setattr(ogun_client, "Client", Client)
    for key, value in {"WORKFLOW": "daily", "PINNED_HASH": "hash", "PARTITION": "2026-01-02", "IDEMPOTENCY_KEY": "retry-key"}.items():
        monkeypatch.setenv(key, value)
    flow = yaml.safe_load((Path(__file__).parents[1] / "flows/ogun-run.yaml").read_text())
    script = next(t["script"] for t in flow["tasks"] if t["id"] == "run")
    if state == "failed":
        with pytest.raises(SystemExit, match="ended failed"):
            exec(compile(script, "kestra-ogun-run", "exec"), {})
    else:
        exec(compile(script, "kestra-ogun-run", "exec"), {})
    output = next(line for line in capsys.readouterr().out.splitlines() if line.startswith("::{"))
    result = json.loads(output[2:-2])["outputs"]
    assert result["state"] == state
    assert result["artifacts"] == ([] if state == "failed" else [{"blake3": "digest", "node": None, "output": None, "partition": None, "size_bytes": None}])
    assert calls[0] == ("daily", {"kind": "keys", "keys": ["2026-01-02"]}, {"idempotency_key": "retry-key", "workflow_hash": "hash"})
    assert calls[-1] == "closed"
