"""Plain data the exporter moves around: events and snapshots.

Both are JSON-serialisable (``to_dict`` / ``from_dict``) because the outbox stores them on disk.
Reading is tolerant: unknown keys are ignored and missing keys take defaults.
"""

from __future__ import annotations

import datetime
import re
import time
from dataclasses import asdict, dataclass, field
from typing import Any, Dict, List, Optional, Tuple

#: Every severity an event can carry. ``attention`` means a person must act.
SEVERITIES = ("info", "warning", "error", "attention")
#: Values a sink's ``min_severity`` may take. ``attention`` always passes.
MIN_SEVERITIES = ("info", "warning", "error")
SEVERITY_RANK = {"info": 0, "warning": 1, "error": 2}

NODE_STATES = (
    "pending",
    "ready",
    "running",
    "waiting",
    "waiting_for_user",
    "waiting_for_session",
    "retry_scheduled",
    "succeeded",
    "failed",
    "cancelled",
    "skipped",
)
RUN_STATES = ("pending", "running", "waiting", "succeeded", "failed", "cancelled")
TERMINAL_NODE_STATES = frozenset({"succeeded", "failed", "cancelled", "skipped"})
TERMINAL_RUN_STATES = frozenset({"succeeded", "failed", "cancelled"})

_TIME = re.compile(
    r"^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?(?:([Zz])|([+-])(\d{2}):(\d{2}))?$"
)


def parse_time_ns(text: Any) -> Optional[int]:
    """RFC 3339 text to nanoseconds since the epoch, or ``None`` when it is not a timestamp."""
    if not isinstance(text, str):
        return None
    m = _TIME.match(text.strip())
    if not m:
        return None
    try:
        y, mo, d, h, mi, s = (int(m.group(i)) for i in range(1, 7))
        stamp = datetime.datetime(y, mo, d, h, mi, s, tzinfo=datetime.timezone.utc)
        secs = int(stamp.timestamp())
    except (ValueError, OverflowError, OSError):
        return None
    if m.group(9):
        offset = int(m.group(10)) * 3600 + int(m.group(11)) * 60
        secs -= offset if m.group(9) == "+" else -offset
    frac = int((m.group(7) or "").ljust(9, "0")[:9])
    return secs * 1_000_000_000 + frac


def format_time(seconds: float) -> str:
    """Seconds since the epoch to RFC 3339 UTC with milliseconds."""
    whole, ms = divmod(int(seconds * 1000), 1000)
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(whole)) + ".%03dZ" % ms


def _ns_or_now(text: str) -> int:
    ns = parse_time_ns(text)
    return ns if ns is not None else time.time_ns()


def _opt_str(value: Any) -> Optional[str]:
    if value is None or isinstance(value, (dict, list, bool)):
        return None
    return str(value)


@dataclass
class ExportEvent:
    """One thing worth telling another system about."""

    kind: str
    at: str
    seq: Optional[int] = None
    run_id: Optional[str] = None
    workflow: Optional[str] = None
    node: Optional[str] = None
    partition: Optional[str] = None
    from_state: Optional[str] = None
    to_state: Optional[str] = None
    cause: Optional[str] = None
    severity: str = "info"
    title: str = ""
    text: str = ""
    tags: Dict[str, str] = field(default_factory=dict)
    dedup_key: Optional[str] = None
    #: The daemon's node instance id; the runner uses it to look up node and partition.
    instance_id: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """A JSON-ready dict."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Any) -> "ExportEvent":
        """Reads a dict written by :meth:`to_dict`; never raises on odd input."""
        d = data if isinstance(data, dict) else {}
        seq = d.get("seq")
        tags = d.get("tags")
        severity = d.get("severity")
        return cls(
            kind=_opt_str(d.get("kind")) or "event",
            at=_opt_str(d.get("at")) or format_time(time.time()),
            seq=seq if isinstance(seq, int) and not isinstance(seq, bool) else None,
            run_id=_opt_str(d.get("run_id")),
            workflow=_opt_str(d.get("workflow")),
            node=_opt_str(d.get("node")),
            partition=_opt_str(d.get("partition")),
            from_state=_opt_str(d.get("from_state")),
            to_state=_opt_str(d.get("to_state")),
            cause=_opt_str(d.get("cause")),
            severity=severity if severity in SEVERITIES else "info",
            title=_opt_str(d.get("title")) or "",
            text=_opt_str(d.get("text")) or "",
            tags={str(k): str(v) for k, v in tags.items()} if isinstance(tags, dict) else {},
            dedup_key=_opt_str(d.get("dedup_key")),
            instance_id=_opt_str(d.get("instance_id")),
        )

    def unix_ns(self) -> int:
        """Event time in nanoseconds; the current time when ``at`` does not parse."""
        return _ns_or_now(self.at)

    def unix_ms(self) -> int:
        """Event time in milliseconds."""
        return self.unix_ns() // 1_000_000

    def unix_s(self) -> int:
        """Event time in whole seconds."""
        return self.unix_ns() // 1_000_000_000

    def facts(self) -> Dict[str, str]:
        """The identifying members that are set, as strings."""
        pairs = (
            ("run_id", self.run_id),
            ("workflow", self.workflow),
            ("node", self.node),
            ("partition", self.partition),
            ("from", self.from_state),
            ("to", self.to_state),
            ("cause", self.cause),
        )
        return {k: v for k, v in pairs if v}


@dataclass
class Snapshot:
    """Daemon counters at one moment; the exporter sends them as gauges."""

    taken_at: str
    workflows: int = 0
    runs_active: int = 0
    approvals_pending: int = 0
    streams: int = 0
    node_instances_by_state: Dict[str, int] = field(default_factory=dict)
    profile: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """A JSON-ready dict."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Any) -> "Snapshot":
        """Reads a dict written by :meth:`to_dict`; never raises on odd input."""
        d = data if isinstance(data, dict) else {}

        def num(key: str) -> int:
            v = d.get(key)
            return v if isinstance(v, int) and not isinstance(v, bool) else 0

        by_state = d.get("node_instances_by_state")
        states: Dict[str, int] = {}
        if isinstance(by_state, dict):
            for k, v in by_state.items():
                if isinstance(v, int) and not isinstance(v, bool):
                    states[str(k)] = v
        return cls(
            taken_at=_opt_str(d.get("taken_at")) or format_time(time.time()),
            workflows=num("workflows"),
            runs_active=num("runs_active"),
            approvals_pending=num("approvals_pending"),
            streams=num("streams"),
            node_instances_by_state=states,
            profile=_opt_str(d.get("profile")),
        )

    def unix_ns(self) -> int:
        """Snapshot time in nanoseconds."""
        return _ns_or_now(self.taken_at)

    def unix_ms(self) -> int:
        """Snapshot time in milliseconds."""
        return self.unix_ns() // 1_000_000

    def unix_s(self) -> int:
        """Snapshot time in whole seconds."""
        return self.unix_ns() // 1_000_000_000

    def gauges(self) -> List[Tuple[str, int, Dict[str, str]]]:
        """``(metric name, value, labels)`` for every gauge; names use dots (``ogun.runs.active``)."""
        out: List[Tuple[str, int, Dict[str, str]]] = [
            ("ogun.workflows", self.workflows, {}),
            ("ogun.runs.active", self.runs_active, {}),
            ("ogun.approvals.pending", self.approvals_pending, {}),
            ("ogun.streams", self.streams, {}),
        ]
        ordered = [s for s in NODE_STATES if s in self.node_instances_by_state]
        ordered += sorted(s for s in self.node_instances_by_state if s not in NODE_STATES)
        for state in ordered:
            out.append(("ogun.node_instances", self.node_instances_by_state[state], {"state": state}))
        return out
