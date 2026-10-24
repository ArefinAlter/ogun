"""``OgunResource``: a Dagster resource that opens ``ogun_client.Client`` connections to ``ogund``.

The resource's configuration names a credential *file*, never a token. A person creates the
credential once on a terminal (``ogun credentials add dagster --kind agent``); the client accepts
only ``agent`` credentials, which can never decide an approval.
"""

from __future__ import annotations

from contextlib import contextmanager
from typing import Any, Callable, Dict, Iterator, Optional

from dagster import ConfigurableResource
from ogun_client import Client
from pydantic import Field


class OgunResource(ConfigurableResource):
    """Connection settings for ``ogund``'s local API."""

    credential: str = Field(default="dagster", description="Credential name under Ogun's credentials/ directory.")
    credential_file: Optional[str] = Field(default=None, description="Explicit path of the token file instead.")
    ogun_home: Optional[str] = Field(default=None, description="Ogun's state directory ($OGUN_HOME); platform default when unset.")
    profile: Optional[str] = Field(default=None, description="Profile whose daemon to reach.")
    timeout_s: float = Field(default=30.0, description="Socket timeout for one call, in seconds.")

    @contextmanager
    def client(self) -> Iterator[Client]:
        """An authenticated client, closed afterwards."""
        c = Client.connect(
            self.credential,
            credential_file=self.credential_file,
            ogun_home=self.ogun_home,
            profile=self.profile,
            client_name="dagster-ogun",
            timeout=self.timeout_s,
        )
        try:
            yield c
        finally:
            c.close()

    def run_and_wait(
        self,
        workflow: str,
        partitions: Optional[Dict[str, Any]],
        *,
        idempotency_key: str,
        workflow_hash: Optional[str] = None,
        version: Optional[int] = None,
        timeout: Optional[float] = None,
        on_update: Optional[Callable[[Dict[str, Any]], None]] = None,
    ) -> Dict[str, Any]:
        """Starts a run and waits for it; returns ``{run, artifacts}``. A run that needs an
        approval waits until a person approves on the Ogun CLI or in Studio."""
        with self.client() as c:
            run = c.start_run(
                workflow, partitions, idempotency_key=idempotency_key, version=version, workflow_hash=workflow_hash
            )
            if on_update is not None:
                on_update(run)
            final = c.wait_for_run(run["run_id"], timeout=timeout, on_update=on_update)
            artifacts = c.list_artifacts(final["run_id"]) if final.get("state") == "succeeded" else []
        return {"run": final, "artifacts": artifacts}
