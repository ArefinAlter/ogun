"""OpenTelemetry: OTLP over HTTP with JSON bodies.

Events become log records (``POST <endpoint>/v1/logs``) and the snapshot becomes gauges
(``POST <endpoint>/v1/metrics``). The resource carries ``service.name=ogun``. 64-bit integers are
JSON strings, as the OTLP JSON mapping asks. A response that reports a partial success is logged
with counts and treated as delivered: OTLP clients must not retry rejected records.
"""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence

from .. import __version__
from ..model import ExportEvent, Snapshot
from ..secrets import Secret
from .base import Field, Sink, host_of

#: OTLP severity numbers and texts.
_SEVERITY = {"info": (9, "INFO"), "warning": (13, "WARN"), "error": (17, "ERROR"), "attention": (13, "WARN")}


def kv(key: str, value: Any) -> Dict[str, Any]:
    """An OTLP key-value attribute with a string value."""
    return {"key": key, "value": {"stringValue": str(value)}}


class OtlpSink(Sink):
    """Posts log records and gauges as OTLP/HTTP JSON."""

    type = "otlp"
    description = "OpenTelemetry OTLP/HTTP JSON: events as logs, snapshot as gauges."
    egress_doc = "host of endpoint"
    capabilities = frozenset({"events", "metrics"})
    HAS_URL = True
    FIELDS = {
        "endpoint": Field("url", required=True, help="base URL, for example http://localhost:4318"),
        "headers_env": Field("env_map", help="header name to {\"env\": \"NAME\"}"),
    }

    def __init__(self, *, endpoint: str, headers_env: Optional[Dict[str, Secret]], **common: Any) -> None:
        super().__init__(**common)
        self.endpoint = endpoint
        self.extra_headers = dict(headers_env or {})

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        host = host_of(cfg.get("endpoint"))
        return [host] if host else []

    @staticmethod
    def _resource() -> Dict[str, Any]:
        return {"attributes": [kv("service.name", "ogun")]}

    @staticmethod
    def _scope() -> Dict[str, Any]:
        return {"name": "ogun-export", "version": __version__}

    def _send(self, path: str, payload: Dict[str, Any]) -> None:
        headers = {name: value.reveal() for name, value in self.extra_headers.items()}
        raw = self._post_json(self.endpoint + path, payload, headers)
        reply = self._json(raw)
        partial = reply.get("partialSuccess") if isinstance(reply, dict) else None
        if isinstance(partial, dict):
            rejected = partial.get("rejectedLogRecords") or partial.get("rejectedDataPoints")
            if rejected not in (None, 0, "0"):
                self.log.warning("sink %s: the collector rejected %s records (partial success)", self.name, rejected)

    @staticmethod
    def log_record(ev: ExportEvent) -> Dict[str, Any]:
        """One OTLP log record."""
        number, text = _SEVERITY[ev.severity]
        attrs = [kv(f"ogun.{k}", v) for k, v in ev.facts().items()]
        attrs.append(kv("ogun.kind", ev.kind))
        attrs.append(kv("ogun.severity", ev.severity))
        if ev.dedup_key:
            attrs.append(kv("ogun.dedup_key", ev.dedup_key))
        ns = str(ev.unix_ns())
        return {
            "timeUnixNano": ns,
            "observedTimeUnixNano": ns,
            "severityNumber": number,
            "severityText": text,
            "body": {"stringValue": f"{ev.title}: {ev.text}" if ev.text else ev.title},
            "attributes": attrs,
        }

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        def chunk(items: Sequence[ExportEvent]) -> None:
            payload = {
                "resourceLogs": [
                    {
                        "resource": self._resource(),
                        "scopeLogs": [{"scope": self._scope(), "logRecords": [self.log_record(e) for e in items]}],
                    }
                ]
            }
            self._send("/v1/logs", payload)

        self._each_chunk(list(events), chunk)

    def send_snapshot(self, snapshot: Snapshot) -> None:
        ns = str(snapshot.unix_ns())
        by_name: Dict[str, List[Dict[str, Any]]] = {}
        for name, value, labels in snapshot.gauges():
            by_name.setdefault(name, []).append(
                {"asInt": str(value), "timeUnixNano": ns, "attributes": [kv(k, v) for k, v in labels.items()]}
            )
        metrics = [{"name": name, "unit": "1", "gauge": {"dataPoints": points}} for name, points in by_name.items()]
        payload = {
            "resourceMetrics": [
                {"resource": self._resource(), "scopeMetrics": [{"scope": self._scope(), "metrics": metrics}]}
            ]
        }
        self._send("/v1/metrics", payload)
