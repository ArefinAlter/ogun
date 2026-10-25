"""Datadog: events (``/api/v1/events``) and gauges (``/api/v2/series``)."""

from __future__ import annotations

import socket
from typing import Any, Dict, List, Mapping, Optional, Sequence

from ..model import ExportEvent, Snapshot
from ..secrets import Secret
from .base import Field, Sink, trunc

SITES = ("datadoghq.com", "datadoghq.eu", "us3.datadoghq.com", "us5.datadoghq.com", "ap1.datadoghq.com", "ddog-gov.com")

#: Datadog's series type for a gauge.
_GAUGE = 3


class DatadogSink(Sink):
    """Posts events and gauge series to the Datadog API."""

    type = "datadog"
    description = "Datadog events and gauge metrics."
    egress_doc = "api.<site>"
    capabilities = frozenset({"events", "metrics"})
    FIELDS = {
        "site": Field("enum", default="datadoghq.com", choices=SITES),
        "api_key": Field("secret", required=True),
        "tags": Field("str_list"),
        "host": Field("str", max_len=255, help="host name attached to metrics; default: this machine's name"),
    }

    def __init__(self, *, site: str, api_key: Secret, tags: Sequence[str], host: Optional[str], **common: Any) -> None:
        super().__init__(**common)
        self.site = site
        self.api_key = api_key
        self.tags = list(tags or [])
        self.host = host or socket.gethostname()
        #: Tests point this at a loopback server.
        self.base_url = f"https://api.{site}"

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        return [f"api.{cfg.get('site') or 'datadoghq.com'}"]

    def _headers(self) -> Dict[str, str]:
        return {"DD-API-KEY": self.api_key.reveal()}

    def _base_tags(self) -> List[str]:
        return ["service:ogun"] + self.tags

    def _event_tags(self, ev: ExportEvent) -> List[str]:
        tags = self._base_tags()
        if ev.workflow:
            tags.append(f"workflow:{ev.workflow}")
        if ev.node:
            tags.append(f"node:{ev.node}")
        return tags

    @staticmethod
    def _alert_type(ev: ExportEvent) -> str:
        if ev.kind == "run_finished" and ev.to_state == "succeeded":
            return "success"
        return {"error": "error", "warning": "warning", "attention": "warning"}.get(ev.severity, "info")

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        def one(ev: ExportEvent) -> None:
            body: Dict[str, Any] = {
                "title": trunc(ev.title, 100),
                "text": trunc(ev.text, 4000),
                "alert_type": self._alert_type(ev),
                "tags": self._event_tags(ev),
                "source_type_name": "ogun",
                "date_happened": ev.unix_s(),
            }
            if ev.dedup_key:
                body["aggregation_key"] = trunc(ev.dedup_key, 100)
            self._post_json(f"{self.base_url}/api/v1/events", body, self._headers())

        self._each(events, one)

    def send_snapshot(self, snapshot: Snapshot) -> None:
        stamp = snapshot.unix_s()
        base = self._base_tags()
        if snapshot.profile:
            base.append(f"profile:{snapshot.profile}")
        series = []
        for name, value, labels in snapshot.gauges():
            series.append(
                {
                    "metric": name,
                    "type": _GAUGE,
                    "points": [{"timestamp": stamp, "value": value}],
                    "tags": base + [f"{k}:{v}" for k, v in labels.items()],
                    "resources": [{"name": self.host, "type": "host"}],
                }
            )
        self._post_json(f"{self.base_url}/api/v2/series", {"series": series}, self._headers())
