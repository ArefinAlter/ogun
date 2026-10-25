"""The exporter loop.

One pass (:meth:`Exporter.run_once`) reads the cursor, asks the daemon for events after it, maps
them, writes them into every interested sink's outbox, and only then persists the cursor. A crash
between the two re-sends events; it never loses them. Snapshots, approvals, run lineage and the
delivery of outbox files follow. The exporter calls only read-only daemon methods: ``status``,
``list_runs``, ``get_run``, ``events_since``, ``list_approvals`` and ``lineage``.

State lives in ``state_dir``: ``cursor.json``, ``open-alerts.json``, ``lineage-sent.json``,
``approvals-pending.json`` and ``outbox/<sink>/``.
"""

from __future__ import annotations

import json
import logging
import os
import threading
import time
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Sequence, Tuple

from . import mapping
from .model import (
    NODE_STATES,
    TERMINAL_NODE_STATES,
    TERMINAL_RUN_STATES,
    ExportEvent,
    Snapshot,
    format_time,
)
from .outbox import Outbox, atomic_write, describe
from .sinks.base import Sink

log = logging.getLogger("ogun_export")

PAGE_SIZE = 1000
MAX_PAGES = 100
MAX_LONG_POLL_MS = 25000
LEDGER_CAP = 1000
ALERT_CAP = 1000
MAX_ACTIVE_RUNS = 100
MAX_BACKOFF_S = 300.0


def is_transient(exc: BaseException) -> bool:
    """Whether a daemon error is the daemon being unreachable (worth retrying the whole step)."""
    if getattr(exc, "code", None) == "api.unavailable":
        return True
    return isinstance(exc, (OSError, EOFError))


class ReconnectingClient:
    """Wraps a client factory and exposes only the read-only calls the exporter makes.

    It connects on first use and reconnects after the daemon went away. It has no ``call`` method
    and no approval or credential methods, so nothing here can decide an approval.
    """

    def __init__(self, factory: Callable[[], Any], initial: Any = None) -> None:
        self._factory = factory
        self._client: Any = initial

    def _call(self, name: str, *args: Any, **kwargs: Any) -> Any:
        if self._client is None:
            self._client = self._factory()
        try:
            return getattr(self._client, name)(*args, **kwargs)
        except Exception as e:
            if is_transient(e):
                self.close()
            raise

    def status(self) -> Any:
        """``daemon.status``."""
        return self._call("status")

    def list_runs(self, workflow: Optional[str] = None, before: Optional[str] = None, limit: Optional[int] = None) -> Any:
        """``runs.list``."""
        return self._call("list_runs", workflow, before, limit)

    def get_run(self, run_id: str) -> Any:
        """``runs.get``."""
        return self._call("get_run", run_id)

    def events_since(self, after: int = 0, run_id: Optional[str] = None, limit: Optional[int] = None, wait_ms: int = 0) -> Any:
        """``events.since``."""
        return self._call("events_since", after=after, run_id=run_id, limit=limit, wait_ms=wait_ms)

    def list_approvals(self, state: Optional[str] = None) -> Any:
        """``approvals.list`` (read-only)."""
        return self._call("list_approvals", state)

    def lineage(self, run_id: Optional[str] = None, blake3_digest: Optional[str] = None) -> Any:
        """``lineage.get``."""
        return self._call("lineage", run_id=run_id, blake3_digest=blake3_digest)

    def close(self) -> None:
        """Closes the connection; the next call reconnects."""
        client, self._client = self._client, None
        closer = getattr(client, "close", None)
        if callable(closer):
            try:
                closer()
            except Exception:  # noqa: BLE001 - closing a dead connection may fail
                pass


class _RunInfo:
    """What one ``get_run`` call tells the runner."""

    def __init__(self, view: Dict[str, Any]) -> None:
        self.view = view
        self.workflow = mapping.clean(view.get("workflow"), 120)
        self.state = view.get("state")
        self.nodes: Dict[str, Tuple[Optional[str], Optional[str]]] = {}
        nodes = view.get("nodes")
        for n in nodes if isinstance(nodes, list) else []:
            if isinstance(n, dict) and isinstance(n.get("instance_id"), str):
                self.nodes[n["instance_id"]] = (mapping.clean(n.get("node"), 120), mapping.clean(n.get("partition"), 120))


class Exporter:
    """Follows the daemon's events and delivers them to sinks through durable outboxes."""

    def __init__(
        self,
        client: Any,
        sinks: Sequence[Sink],
        state_dir: Any,
        interval_s: float = 15,
        snapshot_interval_s: float = 60,
        clock: Callable[[], float] = time.time,
        sleep: Callable[[float], None] = time.sleep,
    ) -> None:
        names = [s.name for s in sinks]
        if len(set(names)) != len(names):
            raise ValueError("sink names must be unique")
        self.client = client
        self.sinks = list(sinks)
        self.interval_s = float(interval_s)
        self.snapshot_interval_s = float(snapshot_interval_s)
        self.clock = clock
        self._sleep = sleep
        self.state_dir = Path(os.path.expanduser(str(state_dir)))
        created = not self.state_dir.exists()
        self.state_dir.mkdir(parents=True, exist_ok=True)
        if created:
            try:
                os.chmod(self.state_dir, 0o700)
            except OSError:
                pass
        self.outbox = Outbox(self.state_dir / "outbox")
        for sink in self.sinks:
            sink.sleep = sleep
            sink.clock = clock
        self._cursor: Optional[int] = None
        self._alerts: Dict[str, str] = {
            k: v for k, v in self._load_dict("open-alerts.json").items() if isinstance(v, str)
        }
        ledger = self._load_dict("lineage-sent.json")
        self._finished: List[str] = [x for x in ledger.get("run_ids", []) if isinstance(x, str)]
        self._lineage_pending: List[str] = [x for x in ledger.get("pending", []) if isinstance(x, str)]
        approvals = self._load_dict("approvals-pending.json")
        self._approvals: Dict[str, Dict[str, Any]] = {k: v for k, v in approvals.items() if isinstance(v, dict)}
        self._last_periodic: Optional[float] = None
        self._failures: Dict[str, int] = {}
        self._next_try: Dict[str, float] = {}
        self._errors: Dict[str, str] = {}
        self._delivered: Dict[str, int] = {s.name: 0 for s in self.sinks}

    # ---- state files ----

    def _load_dict(self, name: str) -> Dict[str, Any]:
        try:
            data = json.loads((self.state_dir / name).read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return {}
        return data if isinstance(data, dict) else {}

    def _save(self, name: str, data: Any) -> None:
        atomic_write(self.state_dir / name, json.dumps(data, separators=(",", ":"), sort_keys=True).encode("utf-8"))

    def _save_state(self) -> None:
        self._save("open-alerts.json", self._alerts)
        self._save("lineage-sent.json", {"run_ids": self._finished, "pending": self._lineage_pending})

    def _read_cursor(self) -> int:
        """The cursor; on first start, the daemon's current ``last_event_seq`` (no history replay)."""
        if self._cursor is not None:
            return self._cursor
        stored = self._load_dict("cursor.json").get("after")
        if isinstance(stored, int) and not isinstance(stored, bool) and stored >= 0:
            self._cursor = stored
            return stored
        status = self.client.status()
        last = status.get("last_event_seq") if isinstance(status, dict) else None
        start = last if isinstance(last, int) and not isinstance(last, bool) and last >= 0 else 0
        self._write_cursor(start)
        log.info("first start: following events after sequence %d", start)
        return start

    def _write_cursor(self, after: int) -> None:
        self._save("cursor.json", {"after": after})
        self._cursor = after

    # ---- helpers ----

    def _sinks_with(self, capability: str) -> List[Sink]:
        return [s for s in self.sinks if capability in s.capabilities]

    def _wanted(self, ev: ExportEvent) -> bool:
        """Whether an event is worth the ``get_run`` call that names its workflow and node."""
        sinks = self._sinks_with("events")
        if not sinks:
            return False
        if ev.severity in ("error", "attention"):
            return True
        return any(s.accepts(ev) for s in sinks)

    def _enqueue_events(self, events: Sequence[ExportEvent]) -> int:
        """Writes each sink's share of ``events`` to its outbox; returns how many events at least one sink took."""
        sinks = self._sinks_with("events")
        for sink in sinks:
            batch = [e for e in events if sink.accepts(e)]
            if batch:
                self.outbox.enqueue(sink.name, "events", [e.to_dict() for e in batch])
        return sum(1 for e in events if any(s.accepts(e) for s in sinks))

    # ---- one pass ----

    def run_once(self, wait_ms: int = 0, *, respect_backoff: bool = False) -> Dict[str, Any]:
        """One pass; never raises for a daemon or sink failure. Returns a summary with
        ``events_seen``, ``events_exported``, ``snapshot``, ``approval_events``, ``daemon_error``,
        ``sink_errors`` and ``pending``."""
        summary: Dict[str, Any] = {
            "events_seen": 0,
            "events_exported": 0,
            "snapshot": False,
            "approval_events": 0,
            "daemon_error": None,
            "sink_errors": {},
            "pending": {},
        }
        try:
            self._pull(wait_ms, summary)
            self._periodic(summary)
            self._lineage_step(summary)
        except Exception as e:  # noqa: BLE001 - reported, then retried by the caller
            summary["daemon_error"] = describe(e)
            log.warning("daemon call failed: %s", describe(e))
        self._drain_all(summary, respect_backoff)
        return summary

    def _pull(self, wait_ms: int, summary: Dict[str, Any]) -> None:
        cursor = self._read_cursor()
        for page in range(MAX_PAGES):
            reply = self.client.events_since(after=cursor, limit=PAGE_SIZE, wait_ms=wait_ms if page == 0 else 0)
            raw = reply.get("events") if isinstance(reply, dict) else None
            raw = raw if isinstance(raw, list) else []
            new_cursor = self._process_page(raw, cursor, summary)
            progressed = new_cursor > cursor
            cursor = new_cursor
            if len(raw) < PAGE_SIZE or not progressed:
                return

    def _process_page(self, raw_events: List[Any], cursor: int, summary: Dict[str, Any]) -> int:
        now = self.clock()
        events: List[ExportEvent] = []
        new_cursor = cursor
        for raw in raw_events:
            ev = mapping.map_event(raw, now=now)
            if ev is None:
                continue
            if ev.seq is not None:
                if ev.seq <= cursor:
                    continue
                new_cursor = max(new_cursor, ev.seq)
            events.append(ev)
        if not events:
            return cursor
        summary["events_seen"] += len(events)

        runs: Dict[str, Optional[_RunInfo]] = {}

        def info(run_id: str) -> Optional[_RunInfo]:
            if run_id not in runs:
                try:
                    view = self.client.get_run(run_id)
                    runs[run_id] = _RunInfo(view) if isinstance(view, dict) else None
                except Exception as e:  # noqa: BLE001
                    if is_transient(e):
                        raise
                    log.warning("get_run failed (%s); the event is sent without workflow and node", describe(e))
                    runs[run_id] = None
            return runs[run_id]

        alerts = dict(self._alerts)
        out: List[ExportEvent] = []
        candidates: Dict[str, ExportEvent] = {}
        for ev in events:
            node_level = ev.kind == "node_transition" and ev.run_id and ev.instance_id
            akey = f"{ev.run_id}/{ev.instance_id}" if node_level else ""
            if node_level and ev.to_state == "succeeded" and akey in alerts:
                ev = mapping.recovered_event(ev, alerts.pop(akey))
            if ev.run_id and self._wanted(ev):
                ri = info(ev.run_id)
                if ri is not None:
                    node, partition = ri.nodes.get(ev.instance_id or "", (None, None))
                    mapping.enrich(ev, ri.workflow, node, partition)
            if node_level and ev.kind == "node_transition" and ev.severity in ("error", "attention"):
                alerts[akey] = ev.dedup_key or akey
            if ev.run_id and (
                (ev.kind == "node_transition" and ev.to_state in TERMINAL_NODE_STATES)
                or (ev.kind == "run_transition" and ev.to_state in TERMINAL_RUN_STATES)
            ):
                candidates[ev.run_id] = ev
            out.append(ev)

        known = set(self._finished)
        announced: List[str] = []
        wanted_finish = bool(self._sinks_with("events") or self._sinks_with("lineage"))
        for run_id, last in candidates.items():
            if run_id in known or not wanted_finish:
                continue
            ri = info(run_id)
            if ri is not None and ri.state in TERMINAL_RUN_STATES:
                out.append(mapping.run_finished_event(ri.view, run_id, last.at, last.seq))
                announced.append(run_id)
        done = known.union(announced)
        # The run-level transition to a terminal state is folded into run_finished.
        out = [e for e in out if not (e.kind == "run_transition" and e.to_state in TERMINAL_RUN_STATES and e.run_id in done)]

        summary["events_exported"] += self._enqueue_events(out)
        self._alerts = dict(list(alerts.items())[-ALERT_CAP:])
        self._finished = (self._finished + announced)[-LEDGER_CAP:]
        if self._sinks_with("lineage"):
            self._lineage_pending = (self._lineage_pending + announced)[-LEDGER_CAP:]
        self._save_state()
        if new_cursor > cursor:
            self._write_cursor(new_cursor)
        return new_cursor

    # ---- snapshots and approvals ----

    def _periodic(self, summary: Dict[str, Any]) -> None:
        now = self.clock()
        if self._last_periodic is not None and now - self._last_periodic < self.snapshot_interval_s:
            return
        if self._sinks_with("metrics"):
            snap = self._snapshot(now)
            for sink in self._sinks_with("metrics"):
                self.outbox.enqueue(sink.name, "snapshot", snap.to_dict())
            summary["snapshot"] = True
        if self._sinks_with("events"):
            summary["approval_events"] += self._approvals_step(now)
        self._last_periodic = now

    def _snapshot(self, now: float) -> Snapshot:
        status = self.client.status()
        status = status if isinstance(status, dict) else {}

        def count(key: str) -> int:
            v = status.get(key)
            if isinstance(v, (list, dict)):
                return len(v)
            return v if isinstance(v, int) and not isinstance(v, bool) else 0

        profile = status.get("profile")
        return Snapshot(
            taken_at=format_time(now),
            workflows=count("workflows"),
            runs_active=count("runs_active"),
            approvals_pending=count("approvals_pending"),
            streams=count("streams"),
            node_instances_by_state=self._count_nodes(),
            profile=profile if isinstance(profile, str) and profile else None,
        )

    def _count_nodes(self) -> Dict[str, int]:
        """Node instances by state over the runs that have not finished (the 100 newest runs are examined)."""
        counts = {s: 0 for s in NODE_STATES}
        try:
            page = self.client.list_runs(limit=100)
            runs = page.get("runs") if isinstance(page, dict) else None
            examined = 0
            for r in runs if isinstance(runs, list) else []:
                if not isinstance(r, dict) or r.get("state") in TERMINAL_RUN_STATES:
                    continue
                if examined >= MAX_ACTIVE_RUNS:
                    break
                examined += 1
                view = r if isinstance(r.get("nodes"), list) else self.client.get_run(str(r.get("run_id")))
                for n in view.get("nodes") or [] if isinstance(view, dict) else []:
                    state = n.get("state") if isinstance(n, dict) else None
                    if isinstance(state, str):
                        counts[state] = counts.get(state, 0) + 1
        except Exception as e:  # noqa: BLE001
            if is_transient(e):
                raise
            log.warning("could not count node instances (%s)", describe(e))
            return {}
        return counts

    def _approvals_step(self, now: float) -> int:
        """Announces approvals that became pending and approvals that stopped being pending."""
        listed = self.client.list_approvals("pending")
        current: Dict[str, Dict[str, Any]] = {}
        for a in listed if isinstance(listed, list) else []:
            if isinstance(a, dict) and a.get("state", "pending") == "pending":
                approval_id = mapping.clean(a.get("approval_id"), 64)
                if approval_id:
                    current[approval_id] = a
        at = format_time(now)
        events: List[ExportEvent] = []
        for approval_id, a in current.items():
            if approval_id not in self._approvals:
                ev = mapping.approval_pending_event(a, at)
                if ev is not None:
                    events.append(ev)
        for approval_id, known in self._approvals.items():
            if approval_id not in current:
                events.append(mapping.approval_resolved_event(approval_id, known, at))
        self._enqueue_events(events)
        self._approvals = {
            k: {"workflow": mapping.clean(v.get("workflow"), 120), "node": mapping.clean(v.get("node"), 120)}
            for k, v in current.items()
        }
        self._save("approvals-pending.json", self._approvals)
        return len(events)

    # ---- lineage ----

    def _lineage_step(self, summary: Dict[str, Any]) -> None:
        sinks = self._sinks_with("lineage")
        if not sinks or not self._lineage_pending:
            return
        remaining = list(self._lineage_pending)
        try:
            for run_id in list(remaining):
                try:
                    items = self.client.lineage(run_id=run_id)
                except Exception as e:  # noqa: BLE001
                    if is_transient(e):
                        raise
                    log.warning("lineage of a run could not be read (%s); skipped", describe(e))
                    remaining.remove(run_id)
                    continue
                batch = [i for i in items if isinstance(i, dict)] if isinstance(items, list) else []
                if batch:
                    for sink in sinks:
                        self.outbox.enqueue(sink.name, "lineage", batch)
                remaining.remove(run_id)
        finally:
            self._lineage_pending = remaining
            self._save_state()

    # ---- delivery ----

    def _drain_all(self, summary: Dict[str, Any], respect_backoff: bool) -> None:
        for sink in self.sinks:
            now = self.clock()
            if respect_backoff and now < self._next_try.get(sink.name, 0.0):
                summary["pending"][sink.name] = self.outbox.count(sink.name)
                continue
            result = self.outbox.drain(sink)
            self._delivered[sink.name] += result.delivered
            summary["pending"][sink.name] = result.pending
            if result.error:
                n = self._failures.get(sink.name, 0) + 1
                self._failures[sink.name] = n
                self._next_try[sink.name] = now + min(MAX_BACKOFF_S, self.interval_s * (2 ** min(n - 1, 8)))
                self._errors[sink.name] = result.error
                summary["sink_errors"][sink.name] = result.error
            else:
                self._failures.pop(sink.name, None)
                self._next_try.pop(sink.name, None)
                self._errors.pop(sink.name, None)

    def status(self) -> Dict[str, Any]:
        """Where the exporter stands: the cursor and, per sink, pending and dropped batches."""
        return {
            "cursor": self._cursor,
            "sinks": {
                s.name: {
                    "type": s.type,
                    "pending": self.outbox.count(s.name),
                    "dropped": self.outbox.dropped(s.name),
                    "delivered_batches": self._delivered.get(s.name, 0),
                    "last_error": self._errors.get(s.name),
                }
                for s in self.sinks
            },
        }

    # ---- the loop ----

    def _pause(self, seconds: float, stop: threading.Event) -> None:
        remaining = seconds
        while remaining > 0 and not stop.is_set():
            step = min(1.0, remaining)
            self._sleep(step)
            remaining -= step

    def run_forever(self, stop: threading.Event) -> None:
        """Loops until ``stop`` is set. Long-polls the daemon; backs off while it is unreachable."""
        failures = 0
        wait_ms = int(min(MAX_LONG_POLL_MS, self.interval_s * 1000))
        while not stop.is_set():
            started = self.clock()
            summary = self.run_once(wait_ms=wait_ms, respect_backoff=True)
            if summary["daemon_error"]:
                failures += 1
                self._pause(min(60.0, 2.0 ** min(failures, 6)), stop)
                continue
            failures = 0
            if summary["events_seen"] == 0 and self.clock() - started < 1.0:
                self._pause(min(1.0, self.interval_s), stop)
