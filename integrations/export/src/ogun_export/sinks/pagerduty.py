"""PagerDuty: Events API v2.

Only severity ``error`` and ``attention`` trigger. When the same node instance later succeeds (the
runner emits a ``node_recovered`` event) or an approval is no longer pending, this sink sends
``resolve`` with the same ``dedup_key``. ``min_severity`` does not apply.
"""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence

from ..model import ExportEvent
from ..secrets import Secret
from .base import Field, Sink, trunc

URLS = {"us": "https://events.pagerduty.com/v2/enqueue", "eu": "https://events.eu.pagerduty.com/v2/enqueue"}
#: Event kinds that close an incident opened by an earlier event with the same ``dedup_key``.
RESOLVE_KINDS = frozenset({"node_recovered", "approval_resolved"})


class PagerDutySink(Sink):
    """Triggers and resolves PagerDuty incidents."""

    type = "pagerduty"
    description = "PagerDuty Events API v2: trigger on error and attention, resolve on recovery."
    egress_doc = "events.pagerduty.com or events.eu.pagerduty.com"
    capabilities = frozenset({"events"})
    MIN_SEVERITY_OK = False
    FIELDS = {
        "region": Field("enum", default="us", choices=tuple(URLS)),
        "routing_key": Field("secret", required=True),
    }

    def __init__(self, *, region: str, routing_key: Secret, **common: Any) -> None:
        super().__init__(**common)
        self.region = region
        self.routing_key = routing_key
        #: Tests point this at a loopback server.
        self.enqueue_url = URLS[region]

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        return ["events.eu.pagerduty.com" if cfg.get("region") == "eu" else "events.pagerduty.com"]

    def accepts(self, event: ExportEvent) -> bool:
        return event.severity in ("error", "attention") or event.kind in RESOLVE_KINDS

    @staticmethod
    def _severity(ev: ExportEvent) -> str:
        return "error" if ev.severity == "error" else "warning"

    def _body(self, ev: ExportEvent) -> Optional[Dict[str, Any]]:
        if ev.kind in RESOLVE_KINDS:
            if not ev.dedup_key:
                return None
            return {"routing_key": self.routing_key.reveal(), "event_action": "resolve", "dedup_key": ev.dedup_key}
        details: Dict[str, Any] = dict(ev.facts())
        details["kind"] = ev.kind
        if ev.text:
            details["text"] = trunc(ev.text, 1000)
        body: Dict[str, Any] = {
            "routing_key": self.routing_key.reveal(),
            "event_action": "trigger",
            "payload": {
                "summary": trunc(ev.title, 1024),
                "source": "ogun",
                "severity": self._severity(ev),
                "custom_details": details,
            },
        }
        if ev.dedup_key:
            body["dedup_key"] = ev.dedup_key
        return body

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        def one(ev: ExportEvent) -> None:
            body = self._body(ev)
            if body is not None:
                self._post_json(self.enqueue_url, body)

        self._each(events, one)
