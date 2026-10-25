"""Prometheus: pushes gauges to a Pushgateway.

``PUT <url>/metrics/job/<job>[/instance/<instance>]`` replaces every metric of that group, which is
what gauges need. The body is the text exposition format, version 0.0.4. The exporter does not
scrape-serve anything; it opens no listener.
"""

from __future__ import annotations

import base64
import urllib.parse
from typing import Any, Dict, List, Mapping, Optional

from ..model import Snapshot
from ..secrets import ConfigError, Secret
from .base import Field, Sink, host_of

CONTENT_TYPE = "text/plain; version=0.0.4; charset=utf-8"
_LABEL_VALUE = r"[A-Za-z0-9_.:-]{1,128}"
_HELP = {
    "ogun_workflows": "Workflows stored in ogund.",
    "ogun_runs_active": "Runs that have not finished.",
    "ogun_approvals_pending": "Approvals waiting for a person.",
    "ogun_streams": "Streams known to ogund.",
    "ogun_node_instances": "Node instances of unfinished runs, by state.",
}


def _escape(value: str) -> str:
    return value.replace("\\", "\\\\").replace("\n", "\\n").replace('"', '\\"')


def exposition(snapshot: Snapshot) -> str:
    """The snapshot as Prometheus text exposition format 0.0.4."""
    lines: List[str] = []
    seen = set()
    for name, value, labels in snapshot.gauges():
        metric = name.replace(".", "_")
        if metric not in seen:
            seen.add(metric)
            lines.append(f"# HELP {metric} {_HELP.get(metric, metric)}")
            lines.append(f"# TYPE {metric} gauge")
        if labels:
            rendered = ",".join(f'{k}="{_escape(v)}"' for k, v in sorted(labels.items()))
            lines.append(f"{metric}{{{rendered}}} {value}")
        else:
            lines.append(f"{metric} {value}")
    return "\n".join(lines) + "\n"


class PrometheusSink(Sink):
    """Pushes gauges with a PUT to the Pushgateway."""

    type = "prometheus"
    description = "Prometheus Pushgateway: gauges in text exposition format."
    egress_doc = "host of url"
    capabilities = frozenset({"metrics"})
    HAS_URL = True
    FIELDS = {
        "url": Field("url", required=True),
        "job": Field("str", default="ogun", pattern=_LABEL_VALUE),
        "instance": Field("str", pattern=_LABEL_VALUE),
        "username": Field("secret", help="basic authentication user"),
        "password": Field("secret", help="basic authentication password"),
    }

    def __init__(
        self,
        *,
        url: str,
        job: str,
        instance: Optional[str],
        username: Optional[Secret],
        password: Optional[Secret],
        **common: Any,
    ) -> None:
        super().__init__(**common)
        self.url = url
        self.job = job
        self.instance = instance
        self.username = username
        self.password = password

    @classmethod
    def cross_check(cls, params: Dict[str, Any], path: str) -> None:
        if (params.get("username") is None) != (params.get("password") is None):
            raise ConfigError(f"{path}: username and password go together")

    @classmethod
    def egress_hosts(cls, cfg: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> List[str]:
        host = host_of(cfg.get("url"))
        return [host] if host else []

    def target(self) -> str:
        """The group URL."""
        path = "/metrics/job/" + urllib.parse.quote(self.job, safe="")
        if self.instance:
            path += "/instance/" + urllib.parse.quote(self.instance, safe="")
        return self.url + path

    def send_snapshot(self, snapshot: Snapshot) -> None:
        headers: Dict[str, str] = {}
        if self.username is not None and self.password is not None:
            pair = f"{self.username.reveal()}:{self.password.reveal()}".encode("utf-8")
            headers["Authorization"] = "Basic " + base64.b64encode(pair).decode("ascii")
        self._post(
            self.target(),
            exposition(snapshot).encode("utf-8"),
            headers,
            method="PUT",
            content_type=CONTENT_TYPE,
        )
