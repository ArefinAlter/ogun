"""``OgunConnection``: a Prefect block with the settings to reach ``ogund``.

The block stores which credential *file* to use, never a token. A person creates the credential
once on a terminal (``ogun credentials add prefect --kind agent``); the client accepts only
``agent`` credentials, which can never decide an approval.
"""

from __future__ import annotations

from contextlib import contextmanager
from typing import Iterator, Optional

from ogun_client import Client
from prefect.blocks.core import Block
from pydantic import Field


class OgunConnection(Block):
    """Where ``ogund`` is and which agent credential file to authenticate with."""

    _block_type_name = "Ogun Connection"
    _description = "Reach ogund through its local API with an agent credential. Holds no secret; never approves."
    _documentation_url = "https://github.com/ArefinAlter/ogun/tree/main/integrations/prefect"

    credential: str = Field(default="prefect", description="Credential name under Ogun's credentials/ directory.")
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
            client_name="prefect-ogun",
            timeout=self.timeout_s,
        )
        try:
            yield c
        finally:
            c.close()
