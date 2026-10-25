"""Slack: an incoming webhook. The URL is the secret; it comes from an environment variable."""

from __future__ import annotations

from typing import Any, List, Mapping, Optional, Sequence

from ..model import ExportEvent
from ..secrets import Secret
from .base import Field, Sink, secret_url_host, trunc


def escape(text: str) -> str:
    """Slack's control characters, as it asks for them to be escaped in message text."""
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


class SlackSink(Sink):
    """Posts one message per event to a Slack incoming webhook."""

    type = "slack"
    description = "Slack incoming webhook messages."
    egress_doc = "host of the webhook URL (hooks.slack.com)"
    capabilities = frozenset({"events"})
    FIELDS = {"webhook_url": Field("secret_url", required=True)}

    def __init__(self, *, webhook_url: Secret, **common: Any) -> None:
        super().__init__(**common)
        self.webhook_url = webhook_url

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        return [secret_url_host(cfg, "webhook_url", environ, "hooks.slack.com")]

    @staticmethod
    def body(ev: ExportEvent) -> dict:
        """The webhook payload: fallback text plus a section and a context block."""
        title = escape(trunc(ev.title, 150))
        text = escape(trunc(ev.text, 2800))
        context = f"severity: {ev.severity}" + (f" | run: {escape(ev.run_id)}" if ev.run_id else "")
        return {
            "text": f"{title}: {text}" if text else title,
            "blocks": [
                {"type": "section", "text": {"type": "mrkdwn", "text": f"*{title}*\n{text}" if text else f"*{title}*"}},
                {"type": "context", "elements": [{"type": "mrkdwn", "text": context}]},
            ],
        }

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        url = self.webhook_url.reveal()
        self._each(events, lambda ev: self._post_json(url, self.body(ev), secret_url=True))
