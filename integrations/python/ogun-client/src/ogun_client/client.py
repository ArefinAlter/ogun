"""The ``ogund`` client: connect, verify the daemon, authenticate, call (O-15).

One connection carries one request at a time; open more clients for concurrency. A client is not
thread-safe.
"""

from __future__ import annotations

import json
import time
from typing import Any, Callable, Dict, Iterator, List, Optional, Sequence, Union

from . import types as t
from .auth import Credential, IpcSecret, new_nonce
from .errors import ForbiddenMethod, OgunError, OgunPermissionDenied, OgunUnauthenticated, OgunUnavailable
from .paths import OgunPaths
from .transport import Transport, connect

PROTOCOL_VERSION = 1

#: Methods this client never sends. Approvals are decided only by a person on the Ogun CLI or
#: in Studio, and trust (credentials, secrets, approver keys, edges, shutdown) is managed only
#: there (Ogun constraint 3). ``ogund`` refuses them for agents anyway; the client refuses first.
FORBIDDEN_METHODS = frozenset(
    {
        "approvals.decide",
        "credentials.add",
        "credentials.list",
        "credentials.revoke",
        "secrets.put",
        "secrets.list",
        "secrets.delete",
        "approvers.propose",
        "approvers.confirm",
        "approvers.revoke",
        "edges.enroll",
        "daemon.shutdown",
    }
)

Workflow = Union[str, bytes, Dict[str, Any]]


def _workflow_value(workflow: Workflow) -> Dict[str, Any]:
    if isinstance(workflow, (str, bytes)):
        value = json.loads(workflow)
    else:
        value = workflow
    if not isinstance(value, dict):
        raise OgunError("api.invalid_params", "a workflow document is a JSON object")
    return value


def _check_key(key: str) -> str:
    if not isinstance(key, str) or not (1 <= len(key) <= 128) or not all(0x20 <= ord(c) < 0x7F for c in key):
        raise OgunError("api.invalid_params", "idempotency_key must be 1 to 128 printable ASCII characters")
    return key


def _drop_none(d: Dict[str, Any]) -> Dict[str, Any]:
    return {k: v for k, v in d.items() if v is not None}


class Client:
    """An authenticated connection to ``ogund``."""

    def __init__(self, transport: Transport, session: Dict[str, Any], hello: Dict[str, Any]) -> None:
        self._transport = transport
        self._next_id = 3
        self._session = session
        self._hello = hello

    # ---- connecting ----

    @classmethod
    def connect(
        cls,
        credential: Optional[str] = None,
        *,
        credential_file: Optional[str] = None,
        paths: Optional[OgunPaths] = None,
        ogun_home: Optional[str] = None,
        profile: Optional[str] = None,
        client_name: str = "ogun-client-python",
        client_version: str = "0.1.0",
        allowed_kinds: Sequence[str] = ("agent",),
        timeout: Optional[float] = 30.0,
    ) -> "Client":
        """Connects with the credential file the caller names.

        ``credential`` is a name under the state directory's ``credentials/`` (``airflow``,
        ``dagster``, ...); ``credential_file`` is an explicit path instead. ``paths``,
        ``ogun_home`` and ``profile`` choose the daemon; by default the same discovery as the
        ``ogun`` CLI (``$OGUN_HOME``, then the platform default).

        ``allowed_kinds`` is the set of credential kinds the caller accepts; the default accepts
        only ``agent``, so an integration never borrows a person's credential.

        Raises :class:`OgunUnavailable` when no daemon listens and :class:`OgunUnauthenticated`
        when the endpoint cannot prove it is this user's daemon (nothing is sent to it then) or
        the credential is refused.
        """
        if credential is None and credential_file is None:
            raise OgunError("api.invalid_params", "name a credential or a credential file")
        if paths is None:
            paths = OgunPaths.rooted(ogun_home, profile) if ogun_home else OgunPaths.discover(profile)
        cred = Credential.read(credential_file or paths.credential_file(credential or ""))
        secret = None
        transport = None
        try:
            secret = IpcSecret.read(paths.ipc_secret_file())
            transport = connect(paths.endpoint(), timeout)
            client_nonce = new_nonce()
            hello = cls._raw_call(
                transport,
                1,
                "daemon.hello",
                {
                    "protocol": [PROTOCOL_VERSION],
                    "client": {"name": client_name, "version": client_version},
                    "client_nonce": client_nonce,
                },
            )
            if not secret.verify_server_proof(client_nonce, str(hello.get("server_proof", ""))):
                raise OgunUnauthenticated("api.unauthenticated", "the endpoint did not prove it is this user's ogund")
            server_nonce = str(hello.get("server_nonce", ""))
            session = cls._raw_call(
                transport,
                2,
                "daemon.authenticate",
                {"credential_id": cred.id, "proof": cred.proof(server_nonce, client_nonce)},
            )
            if session.get("kind") not in allowed_kinds:
                raise OgunPermissionDenied(
                    "api.permission_denied",
                    f"credential kind {session.get('kind')} is not accepted here; use an agent credential",
                )
        except BaseException:
            if transport is not None:
                transport.close()
            raise
        finally:
            cred.wipe()
            if secret is not None:
                secret.wipe()
        return cls(transport, session, hello)

    @staticmethod
    def _raw_call(transport: Transport, rid: int, method: str, params: Any) -> Any:
        transport.send_line(
            json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}, separators=(",", ":")).encode(
                "utf-8"
            )
        )
        line = transport.read_line()
        if line is None:
            raise OgunUnavailable("api.unavailable", "ogund closed the connection")
        try:
            response = json.loads(line)
        except ValueError:
            raise OgunError("internal.bug", "ogund sent a line that is not JSON") from None
        if response.get("id") != rid:
            raise OgunError("internal.bug", "response id does not match the request")
        if response.get("error"):
            raise OgunError.from_rpc(response["error"])
        return response.get("result")

    # ---- session ----

    @property
    def kind(self) -> str:
        """The client kind the credential fixes (``agent`` for integrations)."""
        return str(self._session.get("kind"))

    @property
    def name(self) -> str:
        """The credential's label."""
        return str(self._session.get("name"))

    @property
    def interactive(self) -> bool:
        """Whether the daemon saw an interactive peer (never for a Python program)."""
        return bool(self._session.get("interactive"))

    @property
    def methods(self) -> List[str]:
        """Methods this connection may call."""
        return list(self._session.get("methods") or [])

    @property
    def daemon_version(self) -> str:
        """The daemon's version."""
        return str(self._hello.get("daemon_version"))

    def call(self, method: str, params: Optional[Dict[str, Any]] = None) -> Any:
        """Calls a method with JSON parameters and returns the decoded result."""
        if method in FORBIDDEN_METHODS:
            raise ForbiddenMethod(
                "api.permission_denied",
                f"{method} is decided or managed only by a person on the Ogun CLI or in Studio",
            )
        if self._transport.closed:
            raise OgunUnavailable("api.unavailable", "the connection is closed")
        rid = self._next_id
        self._next_id += 1
        try:
            return self._raw_call(self._transport, rid, method, params if params is not None else {})
        except OgunUnavailable:
            self.close()
            raise

    def close(self) -> None:
        """Closes the connection."""
        self._transport.close()

    def __enter__(self) -> "Client":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    # ---- typed helpers, one per method an agent may call ----

    def status(self) -> t.DaemonStatus:
        """``daemon.status``."""
        return self.call("daemon.status")

    def validate_workflow(self, workflow: Workflow) -> Dict[str, Any]:
        """``workflows.validate``: ``ok``, ``workflow_hash?``, ``diagnostics``."""
        return self.call("workflows.validate", {"workflow": _workflow_value(workflow)})

    def put_workflow(self, workflow: Workflow, idempotency_key: str) -> t.PutWorkflowResult:
        """``workflows.put``. The result lists the approvals the version needs; a person decides them."""
        return self.call(
            "workflows.put", {"workflow": _workflow_value(workflow), "idempotency_key": _check_key(idempotency_key)}
        )

    def list_workflows(self) -> List[t.WorkflowSummary]:
        """``workflows.list``."""
        return self.call("workflows.list")["workflows"]

    def get_workflow(self, name: str, version: Optional[int] = None) -> Dict[str, Any]:
        """``workflows.get``: ``summary``, ``version``, ``workflow_hash``, ``workflow`` (normal form)."""
        return self.call("workflows.get", _drop_none({"name": name, "version": version}))

    def start_run(
        self,
        workflow: str,
        partitions: Optional[Dict[str, Any]] = None,
        *,
        idempotency_key: str,
        version: Optional[int] = None,
        workflow_hash: Optional[str] = None,
    ) -> t.RunView:
        """``runs.start``. Pass ``workflow_hash`` to bind the run to the version you reviewed.
        A version that needs an approval waits for a person (``waiting``)."""
        return self.call(
            "runs.start",
            _drop_none(
                {
                    "workflow": workflow,
                    "version": version,
                    "workflow_hash": workflow_hash,
                    "partitions": partitions or t.latest(),
                    "idempotency_key": _check_key(idempotency_key),
                }
            ),
        )

    def get_run(self, run_id: str) -> t.RunView:
        """``runs.get``: the run with its node instances."""
        return self.call("runs.get", {"run_id": run_id})

    def list_runs(
        self, workflow: Optional[str] = None, before: Optional[str] = None, limit: Optional[int] = None
    ) -> Dict[str, Any]:
        """``runs.list``: ``runs`` newest first, and ``next`` for the next page."""
        return self.call("runs.list", _drop_none({"workflow": workflow, "before": before, "limit": limit}))

    def iter_runs(self, workflow: Optional[str] = None, page_size: int = 100) -> Iterator[t.RunView]:
        """Every run, newest first, following ``next``."""
        before = None
        while True:
            page = self.list_runs(workflow, before, page_size)
            yield from page.get("runs", [])
            before = page.get("next")
            if not before:
                return

    def cancel_run(self, run_id: str, idempotency_key: str) -> t.RunView:
        """``runs.cancel``."""
        return self.call("runs.cancel", {"run_id": run_id, "idempotency_key": _check_key(idempotency_key)})

    def plan_backfill(self, workflow: str, first: str, last: str) -> t.BackfillPlan:
        """``backfills.plan`` over ``first..last``."""
        return self.call("backfills.plan", {"workflow": workflow, "from": first, "to": last})

    def partition_status(
        self, workflow: str, version: Optional[int] = None, first: Optional[str] = None, last: Optional[str] = None
    ) -> t.PartitionStatus:
        """``partitions.status``: range-compressed states for the workflow hash."""
        return self.call(
            "partitions.status", _drop_none({"workflow": workflow, "version": version, "from": first, "to": last})
        )

    def partition_state(self, workflow: str, key: str, version: Optional[int] = None) -> Optional[str]:
        """The state of one partition key (``done``, ``running``, ``failed``, ``missing``), or
        ``None`` when the daemon reports nothing for it."""
        status = self.partition_status(workflow, version, key, key)
        for r in status.get("ranges", []):
            if r.get("from", "") <= key <= r.get("to", ""):
                return r.get("state")
        return None

    def events_since(
        self, after: int = 0, run_id: Optional[str] = None, limit: Optional[int] = None, wait_ms: int = 0
    ) -> Dict[str, Any]:
        """``events.since``: ``events`` and ``last_seq``; long-polls up to ``wait_ms`` (max 30 000)."""
        return self.call(
            "events.since", _drop_none({"after": after, "run_id": run_id, "limit": limit, "wait_ms": min(wait_ms, 30000)})
        )

    def list_approvals(self, state: Optional[str] = None) -> List[t.ApprovalView]:
        """``approvals.list``. Read-only: a person decides on the Ogun CLI or in Studio."""
        return self.call("approvals.list", _drop_none({"state": state}))["approvals"]

    def get_approval(self, approval_id: str) -> t.ApprovalView:
        """``approvals.get``. Read-only."""
        return self.call("approvals.get", {"approval_id": approval_id})

    def list_artifacts(self, run_id: str) -> List[t.ArtifactView]:
        """``artifacts.list``: what a run committed, with BLAKE3 digests."""
        return self.call("artifacts.list", {"run_id": run_id})["artifacts"]

    def get_artifact(self, blake3_digest: str, inline_max_bytes: int = 0) -> Dict[str, Any]:
        """``artifacts.get``: metadata, local path and, when small enough, the bytes in base64."""
        return self.call("artifacts.get", {"blake3": blake3_digest, "inline_max_bytes": inline_max_bytes})

    def lineage(self, run_id: Optional[str] = None, blake3_digest: Optional[str] = None) -> List[Dict[str, Any]]:
        """``lineage.get``: OpenLineage run events of a run or an artifact."""
        return self.call("lineage.get", _drop_none({"run_id": run_id, "blake3": blake3_digest}))["events"]

    def list_streams(self) -> List[t.StreamView]:
        """``streams.list``."""
        return self.call("streams.list")["streams"]

    def stream_status(self, workflow: str, node: str) -> t.StreamView:
        """``streams.status``."""
        return self.call("streams.status", {"workflow": workflow, "node": node})

    # ---- waiting ----

    def wait_for_run(
        self,
        run_id: str,
        timeout: Optional[float] = None,
        poll_interval: float = 5.0,
        on_update: Optional[Callable[[t.RunView], None]] = None,
    ) -> t.RunView:
        """Blocks until the run is terminal and returns its final view.

        It long-polls ``events.since`` for the run and re-reads it on every event. A run that
        waits for an approval keeps waiting: only a person can approve it. Raises
        :class:`OgunError` with code ``api.timeout`` when ``timeout`` (seconds) passes first.
        """
        deadline = None if timeout is None else time.monotonic() + timeout
        run = self.get_run(run_id)
        last_seq = 0
        last_state = None
        while True:
            if run.get("state") != last_state:
                last_state = run.get("state")
                if on_update is not None:
                    on_update(run)
            if run.get("state") in t.TERMINAL_RUN_STATES:
                return run
            remaining = None if deadline is None else deadline - time.monotonic()
            if remaining is not None and remaining <= 0:
                raise OgunError("api.timeout", f"run {run_id} is still {run.get('state')}")
            wait_ms = int(min(poll_interval, remaining if remaining is not None else poll_interval) * 1000)
            try:
                ev = self.events_since(after=last_seq, run_id=run_id, wait_ms=max(wait_ms, 0))
                last_seq = int(ev.get("last_seq", last_seq))
            except OgunPermissionDenied:
                time.sleep(max(wait_ms, 0) / 1000.0)
            run = self.get_run(run_id)
