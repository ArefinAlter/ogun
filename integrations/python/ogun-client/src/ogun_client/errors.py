"""Errors raised by the client.

Every error from ``ogund`` carries the O-00 envelope: a stable ``code`` (``<namespace>.<reason>``)
and a ``message`` for people. Messages never contain secrets; the client never adds a token, a
nonce or a proof to an error.
"""

from __future__ import annotations

from typing import Any, Dict, Optional


class OgunError(Exception):
    """An error from ``ogund`` or from the client itself."""

    def __init__(
        self,
        code: str,
        message: str,
        rpc_code: Optional[int] = None,
        envelope: Optional[Dict[str, Any]] = None,
    ) -> None:
        super().__init__(f"{code}: {message}")
        #: Stable code, for example ``api.unavailable`` or ``ir.invalid``.
        self.code = code
        #: Explanation for people.
        self.message = message
        #: The JSON-RPC error code, when the error came from the daemon.
        self.rpc_code = rpc_code
        #: The full O-00 envelope, when the error came from the daemon.
        self.envelope = envelope or {}

    @classmethod
    def from_rpc(cls, error: Dict[str, Any]) -> "OgunError":
        """Builds the right subclass from a JSON-RPC ``error`` member."""
        data = error.get("data") if isinstance(error.get("data"), dict) else {}
        code = str(data.get("code") or "internal.bug")
        message = str(data.get("message") or error.get("message") or "error")
        kind = _BY_CODE.get(code, OgunError)
        return kind(code, message, error.get("code"), data)


class OgunUnavailable(OgunError):
    """No daemon answers at the endpoint, or the connection dropped (``api.unavailable``)."""


class OgunUnauthenticated(OgunError):
    """The daemon could not prove itself, or refused the credential (``api.unauthenticated``)."""


class OgunPermissionDenied(OgunError):
    """The credential's kind may not call the method (``api.permission_denied``)."""


class ForbiddenMethod(OgunError):
    """The client refuses to send this method: it decides approvals or manages trust.

    Approvals are decided only by a person on the Ogun CLI or in Studio (Ogun constraint 3);
    credentials, secrets, approver keys and edges are managed only there too.
    """


_BY_CODE = {
    "api.unavailable": OgunUnavailable,
    "api.unauthenticated": OgunUnauthenticated,
    "api.permission_denied": OgunPermissionDenied,
}
