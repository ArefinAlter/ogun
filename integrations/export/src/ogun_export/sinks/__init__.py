"""The sinks: one class per external system, registered by ``type``."""

from __future__ import annotations

from typing import Dict, List, Type

from .base import Sink
from .datadog import DatadogSink
from .dingtalk import DingTalkSink
from .elastic import ElasticSink
from .feishu import FeishuSink
from .loki import LokiSink
from .newrelic import NewRelicSink
from .openlineage import OpenLineageSink
from .otlp import OtlpSink
from .pagerduty import PagerDutySink
from .prometheus import PrometheusSink
from .slack import SlackSink
from .splunk import SplunkSink
from .teams import TeamsSink
from .webhook import WebhookSink
from .wecom import WeComSink

#: Every sink class by its ``type`` name, in catalogue order.
REGISTRY: Dict[str, Type[Sink]] = {
    cls.type: cls
    for cls in (
        DatadogSink,
        OpenLineageSink,
        SlackSink,
        TeamsSink,
        PagerDutySink,
        DingTalkSink,
        FeishuSink,
        WeComSink,
        WebhookSink,
        SplunkSink,
        PrometheusSink,
        ElasticSink,
        OtlpSink,
        NewRelicSink,
        LokiSink,
    )
}


def sink_types() -> List[str]:
    """The names of all sink types."""
    return list(REGISTRY)


__all__ = ["REGISTRY", "Sink", "sink_types"]
