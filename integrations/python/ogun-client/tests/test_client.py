import tempfile

import pytest

from ogun_client import Client, ForbiddenMethod, OgunError, OgunPermissionDenied, OgunUnauthenticated, OgunUnavailable, key_range
from ogun_client.types import idempotency_key

from fake_daemon import FakeDaemon


def short_tmp():
    # Unix socket paths must stay short.
    return tempfile.TemporaryDirectory(dir="/tmp", prefix="oc-")


def test_handshake_calls_and_wait():
    states = iter(["running", "running", "succeeded"])
    started = {}

    def start(p):
        started.update(p)
        return {"run_id": "orun_01J9Z3K4M5N6P7Q8R9S0T1V2W3", "state": "pending", "workflow": p["workflow"]}

    handlers = {
        "runs.start": start,
        "runs.get": lambda p: {"run_id": p["run_id"], "state": next(states)},
        "events.since": lambda p: {"events": [], "last_seq": 7},
        "partitions.status": lambda p: {"workflow_hash": "h", "ranges": [{"from": "2026-01-01", "to": "2026-01-31", "count": 31, "state": "done"}]},
    }
    with short_tmp() as d:
        daemon = FakeDaemon(d, handlers)
        with Client.connect("test", ogun_home=d) as c:
            assert c.kind == "agent"
            run = c.start_run("noaa", key_range("2026-01-01", "2026-01-31"), idempotency_key=idempotency_key("airflow", "x", "y"))
            assert started["partitions"] == {"kind": "range", "from": "2026-01-01", "to": "2026-01-31", "force": False}
            assert started["idempotency_key"] == "airflow:x:y"
            final = c.wait_for_run(run["run_id"], timeout=10, poll_interval=0.01)
            assert final["state"] == "succeeded"
            assert c.partition_state("noaa", "2026-01-15") == "done"
            with pytest.raises(ForbiddenMethod):
                c.call("approvals.decide", {"approval_id": "x"})
            with pytest.raises(OgunError):
                c.call("nope.method")
        assert daemon.calls[:2] == ["daemon.hello", "daemon.authenticate"]
        assert "approvals.decide" not in daemon.calls
        daemon.close()


def test_refuses_a_daemon_without_the_secret_and_human_credentials():
    with short_tmp() as d:
        daemon = FakeDaemon(d, {})
        daemon.secret = bytes(32)  # the endpoint no longer proves it holds this user's secret
        with pytest.raises(OgunUnauthenticated):
            Client.connect("test", ogun_home=d)
        assert "daemon.authenticate" not in daemon.calls
        daemon.close()
    with short_tmp() as d:
        daemon = FakeDaemon(d, {}, kind="human", name="cli")
        with pytest.raises(OgunPermissionDenied):
            Client.connect("cli", ogun_home=d)
        daemon.close()


def test_idempotency_keys_are_bounded():
    assert idempotency_key("a", 1) == "a:1"
    long = idempotency_key("x" * 200)
    assert long.startswith("k1-") and len(long) <= 128
    assert idempotency_key("é") != "é"


def test_no_daemon_is_unavailable():
    with short_tmp() as d:
        FakeDaemon(d, {}).close()
        with pytest.raises(OgunUnavailable):
            Client.connect("test", ogun_home=d)
