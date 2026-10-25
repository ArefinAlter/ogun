"""A durable per-sink outbox: at-least-once delivery across failures and restarts.

``<root>/<sink name>/`` holds one JSON file per pending batch, ``{"kind": "events" | "snapshot" |
"lineage", "payload": ...}``, named by a zero-padded counter that only grows. A batch is written
atomically (temporary file, then ``os.replace``) before any send is attempted and deleted after
the send succeeded. :meth:`Outbox.drain` retries the oldest batch first and stops at the first
failure, so order is kept. At most 1000 files wait per sink; beyond that the oldest are dropped and
counted.
"""

from __future__ import annotations

import json
import logging
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional

from .http import DeliveryError
from .model import ExportEvent, Snapshot
from .sinks.base import NAME_RE, Sink

log = logging.getLogger("ogun_export.outbox")

MAX_PENDING = 1000
_BATCH = re.compile(r"^(\d{12})\.json$")
_META = ".meta.json"
KINDS = ("events", "snapshot", "lineage")
#: The sink capability each batch kind needs.
CAPABILITY = {"events": "events", "snapshot": "metrics", "lineage": "lineage"}


def atomic_write(path: Path, data: bytes) -> None:
    """Writes ``data`` so that a reader sees the old file or the new one, never half of it."""
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        with open(tmp, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        try:
            os.remove(tmp)
        except OSError:
            pass
        raise


def describe(exc: BaseException) -> str:
    """The exception's type and stable ``code`` if it has one; never its message."""
    code = getattr(exc, "code", None)
    name = type(exc).__name__
    return f"{name} ({code})" if isinstance(code, str) else name


@dataclass
class DrainResult:
    """What one :meth:`Outbox.drain` did."""

    delivered: int = 0
    error: Optional[str] = None
    pending: int = 0


class Outbox:
    """Directories of pending batches, one per sink."""

    def __init__(self, root: Path, cap: int = MAX_PENDING) -> None:
        self.root = Path(root)
        self.cap = cap
        self._next: Dict[str, int] = {}
        self._dropped: Dict[str, int] = {}

    # ---- files ----

    def _dir(self, sink: str) -> Path:
        if not NAME_RE.match(sink):
            raise ValueError("invalid sink name")
        d = self.root / sink
        d.mkdir(parents=True, exist_ok=True)
        return d

    def pending(self, sink: str) -> List[Path]:
        """Pending batch files, oldest first."""
        d = self._dir(sink)
        return sorted(p for p in d.iterdir() if _BATCH.match(p.name))

    def count(self, sink: str) -> int:
        """How many batches wait."""
        return len(self.pending(sink))

    def dropped(self, sink: str) -> int:
        """How many batches were dropped because the cap was reached (survives restarts)."""
        if sink not in self._dropped:
            try:
                meta = json.loads((self._dir(sink) / _META).read_text(encoding="utf-8"))
                n = meta.get("dropped", 0)
                self._dropped[sink] = n if isinstance(n, int) and n >= 0 else 0
            except (OSError, ValueError, AttributeError):
                self._dropped[sink] = 0
        return self._dropped[sink]

    def _counter(self, sink: str) -> int:
        if sink not in self._next:
            numbers = [int(_BATCH.match(p.name).group(1)) for p in self.pending(sink)]  # type: ignore[union-attr]
            self._next[sink] = (max(numbers) if numbers else 0) + 1
        n = self._next[sink]
        self._next[sink] = n + 1
        return n

    def enqueue(self, sink: str, kind: str, payload: Any) -> Path:
        """Stores one batch durably and returns its file. Call this before sending."""
        if kind not in KINDS:
            raise ValueError("invalid batch kind")
        d = self._dir(sink)
        path = d / f"{self._counter(sink):012d}.json"
        atomic_write(path, json.dumps({"kind": kind, "payload": payload}, separators=(",", ":")).encode("utf-8"))
        self._enforce_cap(sink)
        return path

    def _enforce_cap(self, sink: str) -> None:
        files = self.pending(sink)
        excess = len(files) - self.cap
        if excess <= 0:
            return
        for p in files[:excess]:
            try:
                os.remove(p)
            except OSError:
                pass
        total = self.dropped(sink) + excess
        self._dropped[sink] = total
        atomic_write(self._dir(sink) / _META, json.dumps({"dropped": total}).encode("utf-8"))
        log.warning("sink %s: outbox is full, dropped the %d oldest batches", sink, excess)

    # ---- delivery ----

    @staticmethod
    def _deliver(sink: Sink, kind: str, payload: Any) -> None:
        if kind == "events":
            sink.send_events([ExportEvent.from_dict(d) for d in payload])
        elif kind == "snapshot":
            sink.send_snapshot(Snapshot.from_dict(payload))
        else:
            sink.send_lineage([d for d in payload if isinstance(d, dict)])

    def drain(self, sink: Sink) -> DrainResult:
        """Sends pending batches oldest first and deletes each after success. Stops at the first
        failure so that order is kept; a sink that reported partial progress has its batch rewritten
        without the items it already delivered."""
        result = DrainResult()
        for path in self.pending(sink.name):
            try:
                doc = json.loads(path.read_text(encoding="utf-8"))
                kind = doc["kind"]
                payload = doc["payload"]
                if kind not in KINDS or not isinstance(payload, (list, dict)):
                    raise ValueError("bad batch")
            except (OSError, ValueError, KeyError, TypeError):
                log.warning("sink %s: unreadable outbox file %s set aside", sink.name, path.name)
                try:
                    os.replace(path, path.with_suffix(".bad"))
                except OSError:
                    pass
                continue
            if CAPABILITY[kind] not in sink.capabilities:
                log.warning("sink %s does not support %s; batch discarded", sink.name, kind)
                os.remove(path)
                continue
            try:
                self._deliver(sink, kind, payload)
            except DeliveryError as e:
                result.error = str(e)
                log.warning("sink %s: %s", sink.name, e)
                if e.delivered > 0 and isinstance(payload, list):
                    rest = payload[e.delivered :]
                    atomic_write(path, json.dumps({"kind": kind, "payload": rest}, separators=(",", ":")).encode("utf-8"))
                break
            except Exception as e:  # noqa: BLE001 - a sink bug must not stop the other sinks
                result.error = f"unexpected {describe(e)}"
                log.warning("sink %s: delivery failed with %s", sink.name, describe(e))
                break
            os.remove(path)
            result.delivered += 1
        result.pending = self.count(sink.name)
        return result
