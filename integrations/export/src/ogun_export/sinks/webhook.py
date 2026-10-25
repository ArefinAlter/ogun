"""A generic JSON webhook for everything the exporter has.

Each request body is ``{"type": "events" | "snapshot" | "lineage", "sent_at": <RFC 3339>, ...}``
with ``items`` (events and lineage) or ``snapshot``. With ``signing_secret`` the request carries
``X-Ogun-Signature: sha256=<hex>``, an HMAC-SHA256 over the exact body bytes.
"""

from __future__ import annotations

import hashlib
import hmac
from typing import Any, Dict, List, Mapping, Optional, Sequence

from ..model import ExportEvent, Snapshot, format_time
from ..secrets import ConfigError, Secret
from .base import Field, Sink, dumps, host_of

SIGNATURE_HEADER = "X-Ogun-Signature"
_RESERVED = frozenset({"content-type", "content-length", "host", SIGNATURE_HEADER.lower(), "user-agent"})


class WebhookSink(Sink):
    """Posts batches of events, snapshots and lineage to one URL."""

    type = "webhook"
    description = "Generic JSON webhook: events, snapshots and lineage; optional HMAC signature."
    egress_doc = "host of url"
    capabilities = frozenset({"events", "metrics", "lineage"})
    HAS_URL = True
    CHUNK = 100
    FIELDS = {
        "url": Field("url", required=True),
        "signing_secret": Field("secret"),
        "headers_env": Field("env_map", help="header name to {\"env\": \"NAME\"}, for example an authorization header"),
    }

    def __init__(
        self,
        *,
        url: str,
        signing_secret: Optional[Secret],
        headers_env: Optional[Dict[str, Secret]],
        **common: Any,
    ) -> None:
        super().__init__(**common)
        self.url = url
        self.signing_secret = signing_secret
        self.extra_headers = dict(headers_env or {})

    @classmethod
    def cross_check(cls, params: Dict[str, Any], path: str) -> None:
        for name in params.get("headers_env") or {}:
            if name.lower() in _RESERVED:
                raise ConfigError(f"{path}.headers_env: {name} is set by the exporter and cannot be configured")

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        host = host_of(cfg.get("url"))
        return [host] if host else []

    def _send(self, payload: Dict[str, Any]) -> None:
        body = dumps(payload)
        headers = {name: value.reveal() for name, value in self.extra_headers.items()}
        if self.signing_secret is not None:
            mac = hmac.new(self.signing_secret.reveal().encode("utf-8"), body, hashlib.sha256).hexdigest()
            headers[SIGNATURE_HEADER] = f"sha256={mac}"
        self._post(self.url, body, headers)

    def _sent_at(self) -> str:
        return format_time(self.clock())

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        self._each_chunk(
            list(events),
            lambda chunk: self._send({"type": "events", "sent_at": self._sent_at(), "items": [e.to_dict() for e in chunk]}),
        )

    def send_snapshot(self, snapshot: Snapshot) -> None:
        self._send({"type": "snapshot", "sent_at": self._sent_at(), "snapshot": snapshot.to_dict()})

    def send_lineage(self, items: Sequence[Dict[str, Any]]) -> None:
        self._each_chunk(
            [i for i in items if isinstance(i, dict)],
            lambda chunk: self._send({"type": "lineage", "sent_at": self._sent_at(), "items": list(chunk)}),
        )
