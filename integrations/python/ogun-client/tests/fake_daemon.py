"""A fake ogund for hermetic tests: a Unix socket in a temporary directory that speaks the O-15
handshake and answers a few methods from a table. Loopback only; never a real daemon."""

from __future__ import annotations

import json
import os
import socket
import threading
from typing import Any, Callable, Dict, List

import blake3

from ogun_client.auth import AUTH_KEY_CONTEXT, CLIENT_LABEL, SERVER_LABEL
from ogun_client.paths import OgunPaths

CRED_ID = "ocl_01J9Z3K4M5N6P7Q8R9S0T1V2W3"
AGENT_METHODS = [
    "daemon.status", "workflows.validate", "workflows.put", "workflows.list", "workflows.get",
    "runs.start", "runs.get", "runs.list", "runs.cancel", "backfills.plan", "partitions.status",
    "events.since", "approvals.list", "approvals.get", "artifacts.list", "artifacts.get",
    "lineage.get", "streams.list", "streams.status", "edges.list",
]


class FakeDaemon:
    def __init__(self, base: str, handlers: Dict[str, Callable[[Dict[str, Any]], Any]], kind: str = "agent", name: str = "test") -> None:
        self.paths = OgunPaths.rooted(base, windows=False)
        os.makedirs(self.paths.credentials_dir(), exist_ok=True)
        self.secret = bytes(range(32))
        self.token = bytes(range(32, 64))
        with open(self.paths.ipc_secret_file(), "w") as f:
            f.write(self.secret.hex() + "\n")
        with open(self.paths.credential_file(name), "w") as f:
            f.write(f"{CRED_ID} {kind} {name} ot1_{self.token.hex()}\n")
        self.kind = kind
        self.handlers = handlers
        self.calls: List[str] = []
        sock_path = str(self.paths.endpoint())
        os.makedirs(os.path.dirname(sock_path), exist_ok=True)
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.bind(sock_path)
        self.sock.listen(4)
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self) -> None:
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self._conn, args=(conn,), daemon=True).start()

    def _conn(self, conn: socket.socket) -> None:
        f = conn.makefile("rwb")
        client_nonce = server_nonce = ""
        for line in f:
            req = json.loads(line)
            method, params, rid = req["method"], req.get("params") or {}, req["id"]
            self.calls.append(method)
            try:
                if method == "daemon.hello":
                    client_nonce = params["client_nonce"]
                    server_nonce = "ab" * 32
                    proof = blake3.blake3(SERVER_LABEL + client_nonce.encode(), key=self.secret).hexdigest()
                    result: Any = {"protocol": 1, "daemon_version": "test", "server_nonce": server_nonce, "server_proof": proof}
                elif method == "daemon.authenticate":
                    key = blake3.blake3(self.token, derive_key_context=AUTH_KEY_CONTEXT).digest()
                    want = blake3.blake3(CLIENT_LABEL + CRED_ID.encode() + server_nonce.encode() + client_nonce.encode(), key=key).hexdigest()
                    if params.get("proof") != want:
                        raise LookupError("api.unauthenticated")
                    result = {"kind": self.kind, "name": "test", "interactive": False, "methods": AGENT_METHODS}
                elif method in self.handlers:
                    result = self.handlers[method](params)
                else:
                    raise LookupError("api.method_not_found")
                out = {"jsonrpc": "2.0", "id": rid, "result": result}
            except LookupError as e:
                code = str(e.args[0])
                out = {"jsonrpc": "2.0", "id": rid, "error": {"code": -32000, "message": code, "data": {"code": code, "message": code}}}
            f.write(json.dumps(out).encode() + b"\n")
            f.flush()
        conn.close()

    def close(self) -> None:
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()
        try:
            os.unlink(str(self.paths.endpoint()))
        except OSError:
            pass
