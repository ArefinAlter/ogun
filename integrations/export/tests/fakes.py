"""Test doubles: a loopback HTTP server and a stub with the methods of ``ogun_client.Client``.

Nothing here leaves 127.0.0.1, and nothing sleeps: tests inject a no-op ``sleep``.
"""

from __future__ import annotations

import json
import secrets
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable, Dict, List, Optional, Tuple

from ogun_export.model import ExportEvent, Snapshot


def canary() -> str:
    """A unique marker that must never appear in any message, log or repr."""
    return "OGUN-CANARY-" + secrets.token_hex(8)


class Request:
    """One request the fake server received."""

    def __init__(self, method: str, path: str, headers: Dict[str, str], body: bytes) -> None:
        self.method = method
        self.path = path
        self.headers = headers
        self.body = body

    def header(self, name: str) -> Optional[str]:
        return self.headers.get(name.lower())

    def json(self) -> Any:
        return json.loads(self.body.decode("utf-8"))

    @property
    def route(self) -> str:
        """The path without its query string."""
        return self.path.split("?", 1)[0]

    @property
    def query(self) -> str:
        return self.path.split("?", 1)[1] if "?" in self.path else ""


Reply = Tuple[int, bytes, Dict[str, str]]


class FakeServer:
    """A threaded HTTP server on an ephemeral loopback port that records what it receives.

    Replies come from ``script`` (consumed one per request), then ``responder`` if set, then
    ``default``. A status of 0 drops the connection without a response.
    """

    def __init__(self, responder: Optional[Callable[[Request], Reply]] = None) -> None:
        self.requests: List[Request] = []
        self.script: List[Reply] = []
        self.default: Reply = (200, b"{}", {})
        self.responder = responder
        self._httpd: Optional[ThreadingHTTPServer] = None
        self._thread: Optional[threading.Thread] = None

    def queue(self, status: int, body: bytes = b"", headers: Optional[Dict[str, str]] = None) -> None:
        self.script.append((status, body, headers or {}))

    def set_default(self, status: int, body: bytes = b"", headers: Optional[Dict[str, str]] = None) -> None:
        self.default = (status, body, headers or {})

    def start(self) -> "FakeServer":
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args: Any) -> None:
                pass

            def _handle(self) -> None:
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length) if length else b""
                req = Request(self.command, self.path, {k.lower(): v for k, v in self.headers.items()}, body)
                outer.requests.append(req)
                if outer.script:
                    status, rbody, rheaders = outer.script.pop(0)
                elif outer.responder is not None:
                    status, rbody, rheaders = outer.responder(req)
                else:
                    status, rbody, rheaders = outer.default
                if status == 0:
                    self.close_connection = True
                    try:
                        self.connection.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass
                    return
                self.send_response(status)
                for key, value in rheaders.items():
                    self.send_header(key, value)
                self.send_header("Content-Length", str(len(rbody)))
                self.end_headers()
                self.wfile.write(rbody)

            do_POST = _handle
            do_PUT = _handle
            do_GET = _handle

        self._httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._httpd.daemon_threads = True
        self._thread = threading.Thread(target=self._httpd.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True)
        self._thread.start()
        return self

    @property
    def url(self) -> str:
        assert self._httpd is not None
        return f"http://127.0.0.1:{self._httpd.server_address[1]}"

    def stop(self) -> None:
        if self._httpd is not None:
            self._httpd.shutdown()
            self._httpd.server_close()
            self._httpd = None
        if self._thread is not None:
            self._thread.join(timeout=2)

    def __enter__(self) -> "FakeServer":
        return self.start()

    def __exit__(self, *exc: object) -> None:
        self.stop()


class StubError(Exception):
    """A daemon error with a stable code, like ``ogun_client.OgunError``."""

    def __init__(self, code: str, message: str = "stub error") -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


class StubClient:
    """Has the methods of ``ogun_client.Client`` the exporter may call, and nothing else.

    Any other attribute raises, so a call to an approval-deciding or credential method fails a test.
    """

    kind = "agent"

    def __init__(self) -> None:
        self.events: List[Any] = []
        self.runs: Dict[str, Dict[str, Any]] = {}
        self.approvals: List[Dict[str, Any]] = []
        self.lineages: Dict[str, List[Dict[str, Any]]] = {}
        self.status_overrides: Dict[str, Any] = {}
        self.calls: List[str] = []
        self.events_since_args: List[Dict[str, Any]] = []
        self.fail_events_with: Optional[BaseException] = None
        self.on_events_since: Optional[Callable[[], None]] = None
        self.closed = False

    def __getattr__(self, name: str) -> Any:
        raise AssertionError(f"the exporter called {name}, which a read-only exporter must not")

    def add(self, *events: Any) -> None:
        self.events.extend(events)

    def _max_seq(self) -> int:
        seqs = [e["seq"] for e in self.events if isinstance(e, dict) and isinstance(e.get("seq"), int)]
        return max(seqs) if seqs else 0

    def status(self) -> Dict[str, Any]:
        self.calls.append("status")
        out = {
            "daemon_version": "0.1.0-test",
            "started_at": "2026-10-24T11:00:00.000Z",
            "workflows": 3,
            "runs_active": sum(1 for r in self.runs.values() if r["state"] not in ("succeeded", "failed", "cancelled")),
            "approvals_pending": len(self.approvals),
            "streams": 1,
            "last_event_seq": self._max_seq(),
            "scheduler_enabled": False,
        }
        out.update(self.status_overrides)
        return out

    def list_runs(self, workflow: Optional[str] = None, before: Optional[str] = None, limit: Optional[int] = None) -> Dict[str, Any]:
        self.calls.append("list_runs")
        runs = [{k: v for k, v in r.items() if k != "nodes"} for r in self.runs.values()]
        return {"runs": runs, "next": None}

    def get_run(self, run_id: str) -> Dict[str, Any]:
        self.calls.append("get_run")
        if run_id not in self.runs:
            raise StubError("runs.not_found")
        return json.loads(json.dumps(self.runs[run_id]))

    def events_since(self, after: int = 0, run_id: Optional[str] = None, limit: Optional[int] = None, wait_ms: int = 0) -> Dict[str, Any]:
        self.calls.append("events_since")
        self.events_since_args.append({"after": after, "limit": limit, "wait_ms": wait_ms})
        if self.on_events_since is not None:
            self.on_events_since()
        if self.fail_events_with is not None:
            raise self.fail_events_with
        chosen = [e for e in self.events if not (isinstance(e, dict) and isinstance(e.get("seq"), int) and e["seq"] <= after)]
        if limit:
            chosen = chosen[:limit]
        last = max([after] + [e["seq"] for e in chosen if isinstance(e, dict) and isinstance(e.get("seq"), int)])
        return {"events": chosen, "last_seq": last}

    def list_approvals(self, state: Optional[str] = None) -> List[Dict[str, Any]]:
        self.calls.append("list_approvals")
        return [a for a in self.approvals if state is None or a.get("state") == state]

    def lineage(self, run_id: Optional[str] = None, blake3_digest: Optional[str] = None) -> List[Dict[str, Any]]:
        self.calls.append("lineage")
        return list(self.lineages.get(run_id or "", []))

    def close(self) -> None:
        self.closed = True


# ---- builders ----

AT = "2026-10-24T12:00:00.123Z"


def transition(seq: int, run_id: str, instance_id: Optional[str], to: str, frm: str = "running", cause: Optional[str] = None) -> Dict[str, Any]:
    """A daemon ``transition`` event as it appears on the wire."""
    kind: Dict[str, Any] = {"type": "transition", "from": frm, "to": to, "actor": {"type": "scheduler"}}
    if cause:
        kind["cause"] = cause
    ev: Dict[str, Any] = {"seq": seq, "at": AT, "run_id": run_id, "kind": kind}
    if instance_id:
        ev["instance_id"] = instance_id
    return ev


def run_created(seq: int, run_id: str, instances: int = 4) -> Dict[str, Any]:
    return {"seq": seq, "at": AT, "run_id": run_id, "kind": {"type": "run_created", "instances": instances}}


def node_view(instance_id: str, node: str, partition: str, state: str) -> Dict[str, Any]:
    return {
        "instance_id": instance_id,
        "node": node,
        "partition": partition,
        "state": state,
        "attempt": 1,
        "updated_at": AT,
    }


def run_view(run_id: str, workflow: str, state: str, nodes: List[Dict[str, Any]]) -> Dict[str, Any]:
    return {
        "run_id": run_id,
        "workflow": workflow,
        "version": 1,
        "workflow_hash": "b3:test",
        "state": state,
        "partition_count": len({n["partition"] for n in nodes}),
        "created_at": AT,
        "nodes": nodes,
    }


def failed_event(**overrides: Any) -> ExportEvent:
    ev = ExportEvent(
        kind="node_transition",
        at=AT,
        seq=12,
        run_id="orun_1",
        workflow="noaa-daily",
        node="fetch",
        partition="2026-01-05",
        from_state="running",
        to_state="failed",
        cause="http.timeout",
        severity="error",
        title="noaa-daily/fetch [2026-01-05] failed",
        text="workflow noaa-daily, node fetch; state running -> failed; cause http.timeout",
        tags={"run_id": "orun_1"},
        dedup_key="orun_1:fetch:2026-01-05",
        instance_id="ni_1",
    )
    for k, v in overrides.items():
        setattr(ev, k, v)
    return ev


def attention_event() -> ExportEvent:
    return ExportEvent(
        kind="node_transition",
        at="2026-10-24T12:00:05.500Z",
        seq=13,
        run_id="orun_1",
        workflow="noaa-daily",
        node="review",
        partition="2026-01-06",
        from_state="running",
        to_state="waiting_for_user",
        severity="attention",
        title="noaa-daily/review [2026-01-06] is waiting for a person",
        text="A person must act in the Ogun CLI or in Ogun Studio.",
        dedup_key="orun_1:review:2026-01-06",
        instance_id="ni_2",
    )


def sample_snapshot(profile: Optional[str] = None) -> Snapshot:
    return Snapshot(
        taken_at="2026-10-24T12:01:00.000Z",
        workflows=3,
        runs_active=2,
        approvals_pending=1,
        streams=4,
        node_instances_by_state={"running": 2, "failed": 1},
        profile=profile,
    )


LINEAGE = [
    {"eventType": "START", "eventTime": "2026-10-24T12:00:00.000Z", "run": {"runId": "r-1"}, "job": {"namespace": "ogun", "name": "noaa-daily"}},
    {"eventType": "COMPLETE", "eventTime": "2026-10-24T12:05:00.000Z", "run": {"runId": "r-1"}, "job": {"namespace": "ogun", "name": "noaa-daily"}},
]


def noop_sleep(seconds: float) -> None:
    """Injected wherever the code would sleep."""
