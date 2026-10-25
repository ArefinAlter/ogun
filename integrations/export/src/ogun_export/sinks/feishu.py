"""Feishu (and Lark): a custom bot webhook, plain text messages.

With a signing secret the body carries ``timestamp`` (Unix seconds, as a string) and ``sign``,
which is the base64 of HMAC-SHA256 keyed with ``"<timestamp>\\n<secret>"`` over an empty message.
Success is ``{"code": 0}`` (older bots answer ``{"StatusCode": 0}``); anything else is a failure.
Only the code is reported, never the message text.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
from typing import Any, List, Mapping, Optional, Sequence

from ..model import ExportEvent
from ..secrets import Secret
from .base import Field, Sink, secret_url_host, trunc


class FeishuSink(Sink):
    """Posts one text message per event."""

    type = "feishu"
    description = "Feishu or Lark custom bot (text), optionally signed."
    egress_doc = "open.feishu.cn or open.larksuite.com"
    capabilities = frozenset({"events"})
    FIELDS = {
        "webhook_url": Field("secret_url", required=True),
        "secret": Field("secret", help="signing secret of the bot; adds timestamp and sign to the body"),
    }

    def __init__(self, *, webhook_url: Secret, secret: Optional[Secret], **common: Any) -> None:
        super().__init__(**common)
        self.webhook_url = webhook_url
        self.secret = secret

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        return [secret_url_host(cfg, "webhook_url", environ, "open.feishu.cn")]

    def body(self, ev: ExportEvent) -> dict:
        """The message, signed when a secret is configured."""
        text = f"{trunc(ev.title, 200)}\n{trunc(ev.text, 3000)}".strip()
        body: dict = {}
        if self.secret is not None:
            stamp = str(int(self.clock()))
            key = f"{stamp}\n{self.secret.reveal()}".encode("utf-8")
            body["timestamp"] = stamp
            body["sign"] = base64.b64encode(hmac.new(key, b"", hashlib.sha256).digest()).decode("ascii")
        body["msg_type"] = "text"
        body["content"] = {"text": text}
        return body

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        url = self.webhook_url.reveal()

        def one(ev: ExportEvent) -> None:
            raw = self._post_json(url, self.body(ev), secret_url=True)
            reply = self._json(raw)
            code: Any = None
            if isinstance(reply, dict):
                code = reply["code"] if "code" in reply else reply.get("StatusCode")
            if code != 0 or isinstance(code, bool):
                label = code if isinstance(code, int) and not isinstance(code, bool) else "missing"
                raise self._fail(url, f"Feishu code {label}", status=200, secret_url=True)

        self._each(events, one)
