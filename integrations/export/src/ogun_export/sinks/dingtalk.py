"""DingTalk: a group robot webhook.

DingTalk answers HTTP 200 with ``{"errcode": N, "errmsg": ...}``; ``errcode != 0`` is a failure.
Only the error code is reported, never ``errmsg`` or the URL's query (the access token).
If the robot is set to keyword security, the keyword must appear in the message; every title
here starts with ``Ogun``.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import urllib.parse
from typing import Any, List, Mapping, Optional, Sequence

from ..model import ExportEvent
from ..secrets import Secret
from .base import Field, Sink, secret_url_host, trunc


class DingTalkSink(Sink):
    """Posts one markdown message per event; signs the URL when ``secret`` is set."""

    type = "dingtalk"
    description = "DingTalk group robot (markdown), optionally signed."
    egress_doc = "oapi.dingtalk.com"
    capabilities = frozenset({"events"})
    FIELDS = {
        "webhook_url": Field("secret_url", required=True),
        "secret": Field("secret", help="signing secret of the robot; enables the timestamp and sign parameters"),
    }

    def __init__(self, *, webhook_url: Secret, secret: Optional[Secret], **common: Any) -> None:
        super().__init__(**common)
        self.webhook_url = webhook_url
        self.secret = secret

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        return [secret_url_host(cfg, "webhook_url", environ, "oapi.dingtalk.com")]

    def signed_url(self) -> str:
        """The webhook URL, with ``timestamp`` and ``sign`` appended when a secret is configured."""
        url = self.webhook_url.reveal()
        if self.secret is None:
            return url
        key = self.secret.reveal()
        stamp = str(int(self.clock() * 1000))
        digest = hmac.new(key.encode("utf-8"), f"{stamp}\n{key}".encode("utf-8"), hashlib.sha256).digest()
        sign = urllib.parse.quote_plus(base64.b64encode(digest))
        sep = "&" if "?" in url else "?"
        return f"{url}{sep}timestamp={stamp}&sign={sign}"

    @staticmethod
    def body(ev: ExportEvent) -> dict:
        """The markdown message."""
        title = trunc(f"Ogun: {ev.title}", 100)
        text = f"### {trunc(ev.title, 150)}\n\n{trunc(ev.text, 3000)}\n\nseverity: {ev.severity}"
        return {"msgtype": "markdown", "markdown": {"title": title, "text": text}}

    def send_events(self, events: Sequence[ExportEvent]) -> None:
        def one(ev: ExportEvent) -> None:
            url = self.signed_url()
            raw = self._post_json(url, self.body(ev), secret_url=True)
            reply = self._json(raw)
            code = reply.get("errcode") if isinstance(reply, dict) else None
            if code != 0 or isinstance(code, bool):
                label = code if isinstance(code, int) and not isinstance(code, bool) else "missing"
                raise self._fail(url, f"DingTalk errcode {label}", status=200, secret_url=True)

        self._each(events, one)
