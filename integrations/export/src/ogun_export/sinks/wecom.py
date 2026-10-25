"""WeCom (WeChat Work): a group robot webhook, markdown messages.

Success is ``errcode == 0`` in an HTTP 200 body. Only the code is reported. The markdown content
is cut to 4000 bytes (the service's documented limit is 4096).
"""

from __future__ import annotations

from typing import Any, List, Mapping, Optional, Sequence

from ..model import ExportEvent
from ..secrets import Secret
from .base import Field, Sink, secret_url_host, trunc, trunc_bytes


class WeComSink(Sink):
    """Posts one markdown message per event."""

    type = "wecom"
    description = "WeCom group robot (markdown)."
    egress_doc = "qyapi.weixin.qq.com"
    capabilities = frozenset({"events"})
    FIELDS = {"webhook_url": Field("secret_url", required=True)}

    def __init__(self, *, webhook_url: Secret, **common: Any) -> None:
        super().__init__(**common)
        self.webhook_url = webhook_url

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        return [secret_url_host(cfg, "webhook_url", environ, "qyapi.weixin.qq.com")]

    @staticmethod
    def body(ev: ExportEvent) -> dict:
        """The markdown message."""
        content = f"### {trunc(ev.title, 150)}\n{trunc(ev.text, 3000)}\nseverity: {ev.severity}"
        return {"msgtype": "markdown", "markdown": {"content": trunc_bytes(content, 4000)}}

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        url = self.webhook_url.reveal()

        def one(ev: ExportEvent) -> None:
            reply = self._json(self._post_json(url, self.body(ev), secret_url=True))
            code = reply.get("errcode") if isinstance(reply, dict) else None
            if code != 0 or isinstance(code, bool):
                label = code if isinstance(code, int) and not isinstance(code, bool) else "missing"
                raise self._fail(url, f"WeCom errcode {label}", status=200, secret_url=True)

        self._each(events, one)
