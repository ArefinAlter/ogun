"""``OgunHook``: a connection to ``ogund`` through ``ogun-client``.

The Airflow connection never holds a secret. Its extras name the credential *file* that a person
created with ``ogun credentials add <name> --kind agent``; the token stays in Ogun's state
directory with mode ``0600`` and is read only for the handshake.

Connection extras (all optional):

``credential``
    Credential name under ``credentials/`` (default ``airflow``).
``credential_file``
    Explicit path of the token file instead.
``ogun_home``
    Ogun's state directory, as ``$OGUN_HOME``; default: the platform default for the user the
    worker runs as.
``profile``
    Profile whose daemon to reach.
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional

from ogun_client import Client, OgunError, TERMINAL_RUN_STATES

from .._compat import AirflowException, BaseHook


class OgunHook(BaseHook):
    """Talks to ``ogund`` with an ``agent`` credential. It cannot approve anything."""

    conn_name_attr = "ogun_conn_id"
    default_conn_name = "ogun_default"
    conn_type = "ogun"
    hook_name = "Ogun"

    def __init__(
        self,
        ogun_conn_id: Optional[str] = default_conn_name,
        *,
        credential: Optional[str] = None,
        credential_file: Optional[str] = None,
        ogun_home: Optional[str] = None,
        profile: Optional[str] = None,
        timeout: float = 30.0,
    ) -> None:
        super().__init__()
        self.ogun_conn_id = ogun_conn_id
        self._overrides = {
            "credential": credential,
            "credential_file": credential_file,
            "ogun_home": ogun_home,
            "profile": profile,
        }
        self.timeout = timeout
        self._client: Optional[Client] = None

    @classmethod
    def get_ui_field_behaviour(cls) -> Dict[str, Any]:
        """Hides the fields that would invite a secret; the token never goes into Airflow."""
        return {
            "hidden_fields": ["host", "port", "login", "password", "schema"],
            "relabeling": {},
            "placeholders": {
                "extra": '{"credential": "airflow", "ogun_home": "/var/lib/ogun", "profile": null}',
            },
        }

    def settings(self) -> Dict[str, Any]:
        """Connection extras merged with constructor overrides."""
        extras: Dict[str, Any] = {}
        if self.ogun_conn_id:
            try:
                conn = self.get_connection(self.ogun_conn_id)
                extras = dict(conn.extra_dejson or {})
            except Exception:  # noqa: BLE001 - a missing connection means defaults
                extras = {}
        out = {
            "credential": extras.get("credential") or "airflow",
            "credential_file": extras.get("credential_file"),
            "ogun_home": extras.get("ogun_home"),
            "profile": extras.get("profile"),
        }
        out.update({k: v for k, v in self._overrides.items() if v is not None})
        return out

    def get_conn(self) -> Client:
        """An authenticated client, reused for the hook's lifetime."""
        if self._client is None:
            s = self.settings()
            try:
                self._client = Client.connect(
                    s["credential"],
                    credential_file=s["credential_file"],
                    ogun_home=s["ogun_home"],
                    profile=s["profile"],
                    client_name="airflow-provider-ogun",
                    timeout=self.timeout,
                )
            except OgunError as e:
                raise AirflowException(f"cannot reach ogund: {e}") from None
        return self._client

    def close(self) -> None:
        """Closes the connection."""
        if self._client is not None:
            self._client.close()
            self._client = None

    def _call(self, fn, *args, **kwargs):
        try:
            return fn(self.get_conn(), *args, **kwargs)
        except OgunError as e:
            if e.code == "api.unavailable":
                self.close()
            raise AirflowException(str(e)) from None

    def start_run(
        self,
        workflow: str,
        partitions: Optional[Dict[str, Any]],
        idempotency_key: str,
        version: Optional[int] = None,
        workflow_hash: Optional[str] = None,
    ) -> Dict[str, Any]:
        """``runs.start``."""
        return self._call(
            lambda c: c.start_run(
                workflow, partitions, idempotency_key=idempotency_key, version=version, workflow_hash=workflow_hash
            )
        )

    def get_run(self, run_id: str) -> Dict[str, Any]:
        """``runs.get``."""
        return self._call(lambda c: c.get_run(run_id))

    def cancel_run(self, run_id: str, idempotency_key: str) -> Dict[str, Any]:
        """``runs.cancel``."""
        return self._call(lambda c: c.cancel_run(run_id, idempotency_key))

    def list_artifacts(self, run_id: str) -> List[Dict[str, Any]]:
        """``artifacts.list``."""
        return self._call(lambda c: c.list_artifacts(run_id))

    def partition_state(self, workflow: str, key: str, version: Optional[int] = None) -> Optional[str]:
        """The state of one partition key."""
        return self._call(lambda c: c.partition_state(workflow, key, version))

    def is_terminal(self, run: Dict[str, Any]) -> bool:
        """Whether a run view is final."""
        return run.get("state") in TERMINAL_RUN_STATES

    def test_connection(self):
        """Airflow's connection test: authenticate and read ``daemon.status``."""
        try:
            status = self._call(lambda c: c.status())
            return True, f"ogund {status.get('daemon_version')} is reachable"
        except AirflowException as e:
            return False, str(e)
        finally:
            self.close()
