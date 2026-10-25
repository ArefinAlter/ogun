"""New Relic: the Metric API and the Log API, authenticated with ``Api-Key``.

Daemon counters go to ``metric-api.newrelic.com/metric/v1`` and events to
``log-api.newrelic.com/log/v1``; the EU region uses the ``.eu.`` hosts.
"""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence

from ..model import ExportEvent, Snapshot
from ..secrets import Secret
from .base import Field, Sink

HOSTS = {
    "us": ("metric-api.newrelic.com", "log-api.newrelic.com"),
    "eu": ("metric-api.eu.newrelic.com", "log-api.eu.newrelic.com"),
}
_LEVEL = {"info": "info", "warning": "warn", "error": "error", "attention": "warn"}


class NewRelicSink(Sink):
    """Posts log lines and gauges."""

    type = "newrelic"
    description = "New Relic Log API and Metric API."
    egress_doc = "metric-api.newrelic.com and log-api.newrelic.com (.eu. for region eu)"
    capabilities = frozenset({"events", "metrics"})
    FIELDS = {
        "region": Field("enum", default="us", choices=tuple(HOSTS)),
        "api_key": Field("secret", required=True),
    }

    def __init__(self, *, region: str, api_key: Secret, **common: Any) -> None:
        super().__init__(**common)
        self.region = region
        self.api_key = api_key
        metric_host, log_host = HOSTS[region]
        #: Tests point these at a loopback server.
        self.metric_url = f"https://{metric_host}/metric/v1"
        self.log_url = f"https://{log_host}/log/v1"

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        return list(HOSTS["eu" if cfg.get("region") == "eu" else "us"])

    @staticmethod
    def _common() -> Dict[str, Any]:
        return {"attributes": {"service.name": "ogun"}}

    def _headers(self) -> Dict[str, str]:
        return {"Api-Key": self.api_key.reveal()}

    @staticmethod
    def log_line(ev: ExportEvent) -> Dict[str, Any]:
        """One entry of the ``logs`` array."""
        attrs: Dict[str, str] = {"level": _LEVEL[ev.severity], "kind": ev.kind, "severity": ev.severity}
        attrs.update(ev.facts())
        if ev.dedup_key:
            attrs["dedup_key"] = ev.dedup_key
        return {
            "timestamp": ev.unix_ms(),
            "message": f"{ev.title}: {ev.text}" if ev.text else ev.title,
            "attributes": attrs,
        }

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        def chunk(items: Sequence[ExportEvent]) -> None:
            payload = [{"common": self._common(), "logs": [self.log_line(e) for e in items]}]
            self._post_json(self.log_url, payload, self._headers())

        self._each_chunk(list(events), chunk)

    def send_snapshot(self, snapshot: Snapshot) -> None:
        stamp = snapshot.unix_ms()
        metrics = []
        for name, value, labels in snapshot.gauges():
            entry: Dict[str, Any] = {"name": name, "type": "gauge", "value": value, "timestamp": stamp}
            if labels:
                entry["attributes"] = dict(labels)
            metrics.append(entry)
        self._post_json(self.metric_url, [{"common": self._common(), "metrics": metrics}], self._headers())
