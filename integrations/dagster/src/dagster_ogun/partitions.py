"""Mapping an Ogun workflow's ``partitions`` member (O-02 section 4.2) to a Dagster partitions
definition with the same keys.

* ``time`` with ``day`` granularity: daily partitions, keys ``YYYY-MM-DD``.
* ``time`` with ``month`` granularity: monthly partitions, keys ``YYYY-MM``.
* ``static``: the same keys.
* ``window``: time-window partitions keyed ``wYYYYMMDDTHHMMSSZ`` (O-09). The IR has no first
  window, so the caller passes ``window_start``; the window size must be a whole number of
  minutes that a cron schedule can express.

Ogun's ``end`` is inclusive; Dagster's ``end_date`` is exclusive, so it is moved one step on.
Both list only complete partitions.
"""

from __future__ import annotations

import json
import os
from datetime import date, datetime, timedelta
from typing import Any, Dict, Optional, Union

from dagster import (
    DailyPartitionsDefinition,
    MonthlyPartitionsDefinition,
    PartitionsDefinition,
    StaticPartitionsDefinition,
    TimeWindowPartitionsDefinition,
)

WINDOW_KEY_FORMAT = "w%Y%m%dT%H%M%SZ"


class OgunPartitionsError(ValueError):
    """The workflow's partitions cannot be expressed as Dagster partitions."""


def load_ir(ir: Union[str, os.PathLike, Dict[str, Any]]) -> Dict[str, Any]:
    """A workflow IR document from a dict, a JSON string or a ``.workflow.json`` path."""
    if isinstance(ir, dict):
        return ir
    text = os.fspath(ir)
    if text.lstrip().startswith("{"):
        return json.loads(text)
    with open(text, "r", encoding="utf-8") as f:
        return json.load(f)


def _month(ym: str, step: int = 0) -> datetime:
    y, m = (int(x) for x in ym.split("-"))
    m += step
    y, m = (y + 1, 1) if m == 13 else (y, m)
    return datetime(y, m, 1)


def _day(ymd: str, step: int = 0) -> datetime:
    return datetime.combine(date.fromisoformat(ymd) + timedelta(days=step), datetime.min.time())


def _window_cron(size_ms: int) -> str:
    if size_ms % 60_000 != 0:
        raise OgunPartitionsError("window sizes under a minute have no cron form; pass partitions_def")
    minutes = size_ms // 60_000
    if minutes < 60 and 60 % minutes == 0:
        return f"*/{minutes} * * * *"
    if minutes % 60 == 0 and 24 % (minutes // 60) == 0:
        hours = minutes // 60
        return "0 0 * * *" if hours == 24 else f"0 */{hours} * * *"
    raise OgunPartitionsError("this window size has no cron form; pass partitions_def")


def partitions_def_for(
    ir: Union[str, os.PathLike, Dict[str, Any]],
    *,
    window_start: Optional[Union[str, datetime]] = None,
) -> Optional[PartitionsDefinition]:
    """The Dagster partitions definition for a workflow, or ``None`` when it is unpartitioned."""
    doc = load_ir(ir)
    p = doc.get("partitions")
    if not p:
        return None
    kind = p.get("kind")
    if kind == "time":
        start, end = p["start"], p.get("end")
        # Datetimes, not strings: Dagster parses string bounds with ``fmt``.
        if p.get("granularity") == "day":
            return DailyPartitionsDefinition(start_date=_day(start), end_date=_day(end, 1) if end else None)
        if p.get("granularity") == "month":
            return MonthlyPartitionsDefinition(
                start_date=_month(start), end_date=_month(end, 1) if end else None, fmt="%Y-%m"
            )
        raise OgunPartitionsError("unknown time granularity")
    if kind == "static":
        return StaticPartitionsDefinition(list(p["keys"]))
    if kind == "window":
        if window_start is None:
            raise OgunPartitionsError("window partitions need window_start (the first window to show)")
        start = (
            window_start
            if isinstance(window_start, datetime)
            else datetime.fromisoformat(window_start.replace("Z", "+00:00"))
        )
        return TimeWindowPartitionsDefinition(
            cron_schedule=_window_cron(int(p["size_ms"])),
            start=start,
            fmt=WINDOW_KEY_FORMAT,
            timezone="UTC",
        )
    raise OgunPartitionsError(f"unknown partitions kind {kind}")


def is_window(ir: Optional[Union[str, os.PathLike, Dict[str, Any]]]) -> bool:
    """Whether the workflow's partitions are stream windows, which the stream seals itself."""
    if ir is None:
        return False
    p = load_ir(ir).get("partitions") or {}
    return p.get("kind") == "window"
