"""Elasticsearch: the bulk API.

``POST <url>/_bulk`` with NDJSON, one ``create`` action per document, each with an ``_id`` derived
from the document so that a re-sent batch creates nothing twice (a ``409`` for an existing
document counts as delivered). ``errors: true`` in the response is a failure; the message
reports counts only. Authentication is ``Authorization: ApiKey <value>``; use the ``encoded``
value Elasticsearch shows when it creates the key.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any, Dict, List, Mapping, Optional, Sequence

from ..model import ExportEvent, Snapshot
from ..secrets import Secret
from .base import Field, Sink, host_of

_INDEX = r"[a-z0-9][a-z0-9._-]{0,127}"


class ElasticSink(Sink):
    """Indexes events and snapshots."""

    type = "elastic"
    description = "Elasticsearch bulk API: events and snapshots as documents."
    egress_doc = "host of url"
    capabilities = frozenset({"events", "metrics"})
    HAS_URL = True
    FIELDS = {
        "url": Field("url", required=True),
        "api_key": Field("secret", required=True),
        "index": Field("str", default="ogun-events", pattern=_INDEX),
        "metrics_index": Field("str", default="ogun-metrics", pattern=_INDEX),
    }

    def __init__(self, *, url: str, api_key: Secret, index: str, metrics_index: str, **common: Any) -> None:
        super().__init__(**common)
        self.url = url
        self.api_key = api_key
        self.index = index
        self.metrics_index = metrics_index

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        host = host_of(cfg.get("url"))
        return [host] if host else []

    @staticmethod
    def _line(obj: Dict[str, Any]) -> str:
        return json.dumps(obj, separators=(",", ":"), sort_keys=True, allow_nan=False)

    def _bulk(self, index: str, docs: Sequence[Dict[str, Any]]) -> None:
        lines: List[str] = []
        for doc in docs:
            text = self._line(doc)
            doc_id = hashlib.sha256(text.encode("utf-8")).hexdigest()[:40]
            lines.append(self._line({"create": {"_index": index, "_id": doc_id}}))
            lines.append(text)
        body = ("\n".join(lines) + "\n").encode("utf-8")
        url = self.url + "/_bulk"
        raw = self._post(
            url, body, {"Authorization": f"ApiKey {self.api_key.reveal()}"}, content_type="application/x-ndjson"
        )
        reply = self._json(raw)
        if not isinstance(reply, dict) or not reply.get("errors"):
            return
        items = reply.get("items")
        if not isinstance(items, list):
            raise self._fail(url, "bulk response reported errors", status=200)
        failed = 0
        for item in items:
            result = next(iter(item.values()), None) if isinstance(item, dict) and item else None
            if not isinstance(result, dict):
                continue
            status = result.get("status")
            if isinstance(status, int) and status >= 300 and status != 409:
                failed += 1
            elif status is None and result.get("error"):
                failed += 1
        if failed:
            raise self._fail(url, f"bulk response reported errors: {failed} of {len(items)} items rejected", status=200)

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        self._each_chunk(list(events), lambda chunk: self._bulk(self.index, [self.event_doc(e) for e in chunk]))

    def send_snapshot(self, snapshot: Snapshot) -> None:
        self._bulk(self.metrics_index, [self.snapshot_doc(snapshot)])

    @staticmethod
    def event_doc(ev: ExportEvent) -> Dict[str, Any]:
        """The document for one event."""
        level = {"info": "info", "warning": "warning", "error": "error", "attention": "warning"}[ev.severity]
        ogun = {k: v for k, v in ev.to_dict().items() if v not in (None, "", {}, []) and k not in ("title", "at")}
        return {
            "@timestamp": ev.at,
            "service": {"name": "ogun"},
            "log": {"level": level},
            "event": {"kind": "event", "action": ev.kind},
            "message": f"{ev.title}: {ev.text}" if ev.text else ev.title,
            "ogun": ogun,
        }

    @staticmethod
    def snapshot_doc(snapshot: Snapshot) -> Dict[str, Any]:
        """The document for one snapshot."""
        data = {k: v for k, v in snapshot.to_dict().items() if v not in (None, "") and k != "taken_at"}
        return {"@timestamp": snapshot.taken_at, "service": {"name": "ogun"}, "event": {"kind": "metric"}, "ogun": data}
