"""Daemon event dicts to :class:`ExportEvent`.

Mapping is tolerant: an unknown ``kind.type``, a missing key, a key of the wrong type or an extra
key never raises. Such an event becomes a generic ``event`` of severity ``info``.

Severity rules: a transition to ``failed`` is ``error``; ``retry_scheduled`` is ``warning``;
``waiting_for_user`` is ``attention`` (a person must act); everything else is ``info``. The runner
adds the events that need the daemon's answers: ``run_finished``, ``node_recovered`` and the two
approval events.
"""

from __future__ import annotations

import dataclasses
import time
from collections import Counter
from typing import Any, Dict, Optional

from .model import ExportEvent, format_time, parse_time_ns


def clean(value: Any, limit: int = 200) -> Optional[str]:
    """A short single-line string from a scalar, or ``None``. Control characters become spaces."""
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        text = str(value)
    elif isinstance(value, str):
        text = value
    else:
        return None
    text = "".join(" " if (ord(c) < 32 or ord(c) == 127) else c for c in text).strip()
    return text[:limit] if text else None


def _severity(to_state: Optional[str]) -> str:
    if to_state == "failed":
        return "error"
    if to_state == "retry_scheduled":
        return "warning"
    if to_state == "waiting_for_user":
        return "attention"
    return "info"


def map_event(raw: Any, now: Optional[float] = None) -> Optional[ExportEvent]:
    """Maps one daemon event; ``None`` only when ``raw`` is not an object."""
    if not isinstance(raw, dict):
        return None
    kind = raw.get("kind")
    body: Dict[str, Any] = kind if isinstance(kind, dict) else {}
    etype = clean(body.get("type") if body else kind, 64)
    seq = raw.get("seq")
    seq = seq if isinstance(seq, int) and not isinstance(seq, bool) and seq >= 0 else None
    at = raw.get("at")
    if not isinstance(at, str) or parse_time_ns(at) is None:
        at = format_time(time.time() if now is None else now)
    run_id = clean(raw.get("run_id"), 64)
    instance_id = clean(raw.get("instance_id"), 64)
    ev = ExportEvent(kind="event", at=at.strip(), seq=seq, run_id=run_id, instance_id=instance_id)
    if etype == "transition":
        ev.from_state = clean(body.get("from"), 40)
        ev.to_state = clean(body.get("to"), 40)
        ev.cause = clean(body.get("cause"), 200)
        ev.kind = "node_transition" if instance_id else "run_transition"
        ev.severity = _severity(ev.to_state)
    elif etype == "run_created":
        ev.kind = "run_created"
        count = body.get("instances")
        if isinstance(count, int) and not isinstance(count, bool):
            ev.tags["instances"] = str(count)
    else:
        ev.tags["daemon_event_type"] = etype or "unknown"
    assign_dedup(ev)
    render(ev)
    return ev


def assign_dedup(ev: ExportEvent) -> None:
    """``run_id:node:partition`` for node events, the run id for run events."""
    if ev.kind == "node_transition" and ev.run_id:
        ev.dedup_key = f"{ev.run_id}:{ev.node or ev.instance_id or '-'}:{ev.partition or '-'}"
    elif ev.kind in ("run_transition", "run_created", "run_finished") and ev.run_id:
        ev.dedup_key = ev.run_id


def _subject(ev: ExportEvent) -> str:
    if ev.kind in ("node_transition", "node_recovered"):
        name = f"{ev.workflow}/{ev.node}" if ev.workflow and ev.node else (ev.node or ev.instance_id or "node")
        return f"{name} [{ev.partition}]" if ev.partition else name
    where = f"{ev.workflow} run" if ev.workflow else "run"
    return f"{where} {ev.run_id}" if ev.run_id else where


def _where(ev: ExportEvent) -> str:
    parts = []
    for label, value in (("workflow", ev.workflow), ("node", ev.node), ("partition", ev.partition), ("run", ev.run_id)):
        if value:
            parts.append(f"{label} {value}")
    return ", ".join(parts)


def render(ev: ExportEvent) -> None:
    """Sets ``title``, ``text`` and ``tags`` from the event's fields. Kinds this module does not
    describe keep the title and text they were given."""
    for key, value in (
        ("run_id", ev.run_id),
        ("workflow", ev.workflow),
        ("node", ev.node),
        ("partition", ev.partition),
        ("to", ev.to_state),
        ("cause", ev.cause),
    ):
        if value:
            ev.tags[key] = value
    subject = _subject(ev)
    move = f"{ev.from_state or '?'} -> {ev.to_state or '?'}"
    detail = f"; cause {ev.cause}" if ev.cause else ""
    if ev.kind == "node_transition":
        if ev.to_state == "failed":
            ev.title = f"{subject} failed"
        elif ev.to_state == "retry_scheduled":
            ev.title = f"{subject} will be retried"
        elif ev.to_state == "waiting_for_user":
            ev.title = f"{subject} is waiting for a person"
        else:
            ev.title = f"{subject}: {move}"
        ev.text = f"{_where(ev)}; state {move}{detail}"
        if ev.to_state == "waiting_for_user":
            ev.text += ". A person must act in the Ogun CLI or in Ogun Studio."
    elif ev.kind == "node_recovered":
        ev.title = f"{subject} recovered"
        ev.text = f"{_where(ev)}; state {move}"
    elif ev.kind == "run_created":
        count = ev.tags.get("instances")
        ev.title = f"{subject} created"
        ev.text = _where(ev) + (f"; {count} node instances" if count else "")
    elif ev.kind == "run_transition":
        ev.title = f"{subject}: {move}"
        ev.text = f"{_where(ev)}; state {move}{detail}"
    elif ev.kind == "run_finished":
        ev.title = f"{subject} {ev.to_state}"
    elif ev.kind == "event":
        ev.title = f"Ogun event: {ev.tags.get('daemon_event_type', 'unknown')}"
        ev.text = _where(ev)


def enrich(ev: ExportEvent, workflow: Optional[str], node: Optional[str], partition: Optional[str]) -> None:
    """Fills in what only ``get_run`` knows, then renders again."""
    if workflow:
        ev.workflow = workflow
    if node:
        ev.node = node
    if partition:
        ev.partition = partition
    if ev.kind == "node_transition":
        assign_dedup(ev)
    render(ev)


def recovered_event(ev: ExportEvent, dedup_key: str) -> ExportEvent:
    """A ``node_recovered`` event from the ``succeeded`` transition of an instance that had alerted."""
    out = dataclasses.replace(ev, kind="node_recovered", severity="info", dedup_key=dedup_key, tags=dict(ev.tags))
    render(out)
    return out


def run_finished_event(view: Dict[str, Any], run_id: str, at: str, seq: Optional[int]) -> ExportEvent:
    """The event for a run that reached a terminal state, from its ``get_run`` view."""
    state = clean(view.get("state"), 40) or "unknown"
    ev = ExportEvent(
        kind="run_finished",
        at=at,
        seq=seq,
        run_id=run_id,
        workflow=clean(view.get("workflow"), 120),
        to_state=state,
        severity="error" if state == "failed" else "info",
    )
    assign_dedup(ev)
    render(ev)
    nodes = view.get("nodes")
    counts = Counter(
        str(n.get("state")) for n in (nodes if isinstance(nodes, list) else []) if isinstance(n, dict) and n.get("state")
    )
    parts = [_where(ev)]
    pc = view.get("partition_count")
    if isinstance(pc, int) and not isinstance(pc, bool):
        parts.append(f"{pc} partitions")
    if counts:
        parts.append("nodes: " + ", ".join(f"{s} {n}" for s, n in sorted(counts.items())))
    ev.text = "; ".join(p for p in parts if p)
    ev.tags["to"] = state
    return ev


def approval_pending_event(approval: Dict[str, Any], at: str) -> Optional[ExportEvent]:
    """An ``attention`` event for an approval that is waiting for a person; ``None`` without an id."""
    approval_id = clean(approval.get("approval_id"), 64)
    if not approval_id:
        return None
    akind = clean(approval.get("kind"), 64) or "approval"
    workflow = clean(approval.get("workflow"), 120)
    node = clean(approval.get("node"), 120)
    requested_by = clean(approval.get("requested_by"), 120)
    ev = ExportEvent(
        kind="approval_pending",
        at=at,
        workflow=workflow,
        node=node,
        severity="attention",
        dedup_key=approval_id,
        tags={"approval_id": approval_id, "approval_kind": akind},
    )
    if requested_by:
        ev.tags["requested_by"] = requested_by
    ev.title = f"Approval {approval_id} ({akind}) is waiting for a person"
    bits = [f"workflow {workflow}" if workflow else "", f"node {node}" if node else "", f"requested by {requested_by}" if requested_by else ""]
    ev.text = "; ".join(b for b in bits if b) + (". " if any(bits) else "") + "A person decides it in the Ogun CLI or in Ogun Studio."
    return ev


def approval_resolved_event(approval_id: str, info: Dict[str, Any], at: str) -> ExportEvent:
    """An ``info`` event for an approval that is no longer pending."""
    ev = ExportEvent(
        kind="approval_resolved",
        at=at,
        workflow=clean(info.get("workflow"), 120),
        node=clean(info.get("node"), 120),
        severity="info",
        dedup_key=approval_id,
        tags={"approval_id": approval_id},
    )
    ev.title = f"Approval {approval_id} is no longer pending"
    ev.text = "It was decided or withdrawn."
    return ev
