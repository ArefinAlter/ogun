"""Microsoft Teams: a Workflows (Power Automate) webhook that accepts an Adaptive Card message.

The URL is the secret; it comes from an environment variable. Which host it has depends on the
tenant's Workflows setup, so ``check`` prints the host of the URL you configured.
"""

from __future__ import annotations

from typing import Any, List, Mapping, Optional, Sequence

from ..model import ExportEvent
from ..secrets import Secret
from .base import Field, Sink, secret_url_host, trunc


class TeamsSink(Sink):
    """Posts one Adaptive Card message per event."""

    type = "teams"
    description = "Microsoft Teams Workflows webhook (Adaptive Card message)."
    egress_doc = "host of the webhook URL (depends on the tenant)"
    capabilities = frozenset({"events"})
    FIELDS = {"webhook_url": Field("secret_url", required=True)}

    def __init__(self, *, webhook_url: Secret, **common: Any) -> None:
        super().__init__(**common)
        self.webhook_url = webhook_url

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        return [secret_url_host(cfg, "webhook_url", environ, "<host of the webhook URL>")]

    @staticmethod
    def body(ev: ExportEvent) -> dict:
        """The message with one Adaptive Card attachment."""
        card_body: List[dict] = [
            {"type": "TextBlock", "text": trunc(ev.title, 200), "weight": "Bolder", "size": "Medium", "wrap": True}
        ]
        if ev.text:
            card_body.append({"type": "TextBlock", "text": trunc(ev.text, 2000), "wrap": True})
        facts = [{"title": "Severity", "value": ev.severity}]
        for key, value in ev.facts().items():
            if key in ("workflow", "node", "partition", "run_id"):
                facts.append({"title": key, "value": value})
        card_body.append({"type": "FactSet", "facts": facts})
        return {
            "type": "message",
            "attachments": [
                {
                    "contentType": "application/vnd.microsoft.card.adaptive",
                    "content": {
                        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                        "type": "AdaptiveCard",
                        "version": "1.4",
                        "body": card_body,
                    },
                }
            ],
        }

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        url = self.webhook_url.reveal()
        self._each(events, lambda ev: self._post_json(url, self.body(ev), secret_url=True))
