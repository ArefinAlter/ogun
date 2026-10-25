"""Splunk: the HTTP Event Collector (HEC).

Events go to ``<url>/services/collector/event`` with ``Authorization: Splunk <token>`` as
concatenated JSON objects. Daemon counters are sent as one event of sourcetype ``ogun:snapshot``.
Success is ``{"text": "Success", "code": 0}``. HEC servers often use a self-signed certificate;
this sink does not offer a way to turn certificate checks off.
"""

from __future__ import annotations

import socket
from typing import Any, Dict, List, Mapping, Optional, Sequence

from ..model import ExportEvent, Snapshot
from ..secrets import Secret
from .base import Field, Sink, dumps, host_of

PATH = "/services/collector/event"


def _present(d: Dict[str, Any]) -> Dict[str, Any]:
    return {k: v for k, v in d.items() if v not in (None, "", {}, [])}


class SplunkSink(Sink):
    """Posts events and snapshots to HEC."""

    type = "splunk"
    description = "Splunk HTTP Event Collector: events and a snapshot event."
    egress_doc = "host of url"
    capabilities = frozenset({"events", "metrics"})
    HAS_URL = True
    FIELDS = {
        "url": Field("url", required=True),
        "token": Field("secret", required=True),
        "index": Field("str", max_len=128),
        "host": Field("str", max_len=255, help="host field of each event; default: this machine's name"),
    }

    def __init__(self, *, url: str, token: Secret, index: Optional[str], host: Optional[str], **common: Any) -> None:
        super().__init__(**common)
        self.url = url
        self.token = token
        self.index = index
        self.host = host or socket.gethostname()

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        host = host_of(cfg.get("url"))
        return [host] if host else []

    def _wrap(self, when_ms: int, sourcetype: str, event: Dict[str, Any]) -> Dict[str, Any]:
        doc: Dict[str, Any] = {
            "time": when_ms / 1000.0,
            "host": self.host,
            "source": "ogun",
            "sourcetype": sourcetype,
            "event": event,
        }
        if self.index:
            doc["index"] = self.index
        return doc

    def _send(self, docs: Sequence[Dict[str, Any]]) -> None:
        body = b"".join(dumps(d) for d in docs)
        url = self.url + PATH
        raw = self._post(url, body, {"Authorization": f"Splunk {self.token.reveal()}"})
        reply = self._json(raw)
        code = reply.get("code") if isinstance(reply, dict) else None
        if code != 0 or isinstance(code, bool):
            label = code if isinstance(code, int) and not isinstance(code, bool) else "missing"
            raise self._fail(url, f"HEC code {label}", status=200)

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        self._each_chunk(
            list(events),
            lambda chunk: self._send([self._wrap(e.unix_ms(), "ogun:event", _present(e.to_dict())) for e in chunk]),
        )

    def send_snapshot(self, snapshot: Snapshot) -> None:
        self._send([self._wrap(snapshot.unix_ms(), "ogun:snapshot", _present(snapshot.to_dict()))])
