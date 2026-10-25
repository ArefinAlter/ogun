"""Grafana Loki: the push API.

``POST <url>/loki/api/v1/push`` with one stream per ``(level, workflow)``; each value is a
timestamp in nanoseconds and a JSON line. Authentication is a bearer ``token`` or basic
``username`` and ``password`` from the environment; ``tenant_id`` sets ``X-Scope-OrgID``.
Loki refuses entries older than its configured age limit, so events delayed for long by an
unreachable Loki may be rejected when they finally arrive.
"""

from __future__ import annotations

import base64
import json
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from ..model import ExportEvent
from ..secrets import ConfigError, Secret
from .base import Field, Sink, dumps, host_of

PATH = "/loki/api/v1/push"
_LEVEL = {"info": "info", "warning": "warn", "error": "error", "attention": "warn"}


class LokiSink(Sink):
    """Pushes event lines to Loki."""

    type = "loki"
    description = "Grafana Loki push API: events as JSON log lines."
    egress_doc = "host of url"
    capabilities = frozenset({"events"})
    HAS_URL = True
    FIELDS = {
        "url": Field("url", required=True),
        "token": Field("secret", help="bearer token"),
        "username": Field("secret", help="basic authentication user"),
        "password": Field("secret", help="basic authentication password"),
        "tenant_id": Field("str", max_len=150, help="sent as X-Scope-OrgID"),
    }

    def __init__(
        self,
        *,
        url: str,
        token: Optional[Secret],
        username: Optional[Secret],
        password: Optional[Secret],
        tenant_id: Optional[str],
        **common: Any,
    ) -> None:
        super().__init__(**common)
        self.url = url
        self.token = token
        self.username = username
        self.password = password
        self.tenant_id = tenant_id

    @classmethod
    def cross_check(cls, params: Dict[str, Any], path: str) -> None:
        if (params.get("username") is None) != (params.get("password") is None):
            raise ConfigError(f"{path}: username and password go together")
        if params.get("token") is not None and params.get("username") is not None:
            raise ConfigError(f"{path}: use either token or username and password, not both")

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        host = host_of(cfg.get("url"))
        return [host] if host else []

    def _headers(self) -> Dict[str, str]:
        headers: Dict[str, str] = {}
        if self.token is not None:
            headers["Authorization"] = f"Bearer {self.token.reveal()}"
        elif self.username is not None and self.password is not None:
            pair = f"{self.username.reveal()}:{self.password.reveal()}".encode("utf-8")
            headers["Authorization"] = "Basic " + base64.b64encode(pair).decode("ascii")
        if self.tenant_id:
            headers["X-Scope-OrgID"] = self.tenant_id
        return headers

    @staticmethod
    def streams(events: Sequence[ExportEvent]) -> List[Dict[str, Any]]:
        """Events grouped into streams by level and workflow, each in time order."""
        groups: Dict[Tuple[str, str], List[Tuple[int, str]]] = {}
        for ev in events:
            key = (_LEVEL[ev.severity], ev.workflow or "unknown")
            line = json.dumps({k: v for k, v in ev.to_dict().items() if v not in (None, "", {}, [])}, separators=(",", ":"))
            groups.setdefault(key, []).append((ev.unix_ns(), line))
        out = []
        for (level, workflow), values in groups.items():
            values.sort(key=lambda pair: pair[0])
            out.append(
                {
                    "stream": {"service_name": "ogun", "level": level, "workflow": workflow},
                    "values": [[str(ns), line] for ns, line in values],
                }
            )
        return out

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        def chunk(items: Sequence[ExportEvent]) -> None:
            self._post(self.url + PATH, dumps({"streams": self.streams(items)}), self._headers())

        self._each_chunk(list(events), chunk)
