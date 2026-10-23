"""Python client for ``ogund``, the Ogun daemon (Ogun spec O-15).

::

    from ogun_client import Client, key_range, idempotency_key

    with Client.connect("airflow") as ogun:          # credentials/airflow.token, kind agent
        run = ogun.start_run(
            "noaa-daily",
            key_range("2026-01-01", "2026-01-31"),
            idempotency_key=idempotency_key("airflow", "noaa", "manual__2026-02-01"),
        )
        final = ogun.wait_for_run(run["run_id"])

The client speaks the local API only: a Unix domain socket or a Windows named pipe, a mutual
handshake that proves the endpoint is this user's daemon before anything else is sent, and
newline-delimited JSON-RPC. It never decides an approval and never manages credentials or
secrets; those happen only on the Ogun CLI or in Studio.
"""

from .auth import Credential, IpcSecret
from .client import FORBIDDEN_METHODS, PROTOCOL_VERSION, Client
from .errors import ForbiddenMethod, OgunError, OgunPermissionDenied, OgunUnauthenticated, OgunUnavailable
from .paths import Endpoint, OgunPaths
from .types import TERMINAL_RUN_STATES, idempotency_key, key_range, keys, latest

__version__ = "0.1.0"

__all__ = [
    "Client",
    "Credential",
    "IpcSecret",
    "Endpoint",
    "OgunPaths",
    "OgunError",
    "OgunUnavailable",
    "OgunUnauthenticated",
    "OgunPermissionDenied",
    "ForbiddenMethod",
    "FORBIDDEN_METHODS",
    "PROTOCOL_VERSION",
    "TERMINAL_RUN_STATES",
    "idempotency_key",
    "key_range",
    "keys",
    "latest",
]
