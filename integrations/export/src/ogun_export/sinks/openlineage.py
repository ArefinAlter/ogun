"""OpenLineage: one POST per run event to a lineage server.

The default path, ``/api/v1/lineage``, is Marquez's. Other servers (DataHub, OpenMetadata, Atlan,
and others) take OpenLineage events on other paths and with other authentication; check the path
against the vendor's documentation and set ``path`` yourself. This sink never guesses one.
"""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence

from ..secrets import ConfigError, Secret
from .base import Field, Sink, host_of

_PATH = r"/[A-Za-z0-9._~!$&'()*+,;=:@%/-]*"


class OpenLineageSink(Sink):
    """Posts OpenLineage run events as JSON."""

    type = "openlineage"
    description = "OpenLineage run events to a lineage server (default path: Marquez)."
    egress_doc = "host of url"
    capabilities = frozenset({"lineage"})
    HAS_URL = True
    FIELDS = {
        "url": Field("url", required=True),
        "path": Field("str", default="/api/v1/lineage", pattern=_PATH, max_len=200),
        "auth": Field("secret", help="sent as 'Authorization: Bearer <value>'"),
        "header_name": Field("str", pattern=r"[A-Za-z0-9][A-Za-z0-9-]{0,63}", max_len=64),
        "header_value_env": Field("secret", help="value of the header named by header_name"),
    }

    def __init__(
        self,
        *,
        url: str,
        path: str,
        auth: Optional[Secret],
        header_name: Optional[str],
        header_value_env: Optional[Secret],
        **common: Any,
    ) -> None:
        super().__init__(**common)
        self.url = url
        self.path = path
        self.auth = auth
        self.header_name = header_name
        self.header_value = header_value_env

    @classmethod
    def cross_check(cls, params: Dict[str, Any], path: str) -> None:
        if (params.get("header_name") is None) != (params.get("header_value_env") is None):
            raise ConfigError(f"{path}: header_name and header_value_env go together")

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        host = host_of(cfg.get("url"))
        return [host] if host else []

    def _headers(self) -> Dict[str, str]:
        headers: Dict[str, str] = {}
        if self.auth is not None:
            headers["Authorization"] = f"Bearer {self.auth.reveal()}"
        if self.header_name and self.header_value is not None:
            headers[self.header_name] = self.header_value.reveal()
        return headers

    def send_lineage(self, items: Sequence[Dict[str, Any]]) -> None:
        target = self.url + self.path
        events = [i for i in items if isinstance(i, dict)]
        self._each(events, lambda item: self._post_json(target, item, self._headers()))
