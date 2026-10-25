import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fakes import FakeServer, canary, failed_event, noop_sleep, sample_snapshot
from ogun_export import outbox as outbox_module
from ogun_export.http import DeliveryError
from ogun_export.outbox import Outbox, atomic_write
from ogun_export.sinks import REGISTRY
from ogun_export.sinks.base import Sink


class ScriptedSink(Sink):
    """Records what it is given; fails on demand."""

    type = "scripted"
    capabilities = frozenset({"events", "metrics", "lineage"})

    def __init__(self, name="s1", capabilities=None):
        super().__init__(name=name)
        if capabilities is not None:
            self.capabilities = frozenset(capabilities)
        self.sent = []
        self.fail = None

    def _maybe_fail(self):
        if self.fail is not None:
            raise self.fail

    def send_events(self, events):
        self._maybe_fail()
        self.sent.append(("events", [e.seq for e in events]))

    def send_snapshot(self, snapshot):
        self._maybe_fail()
        self.sent.append(("snapshot", snapshot.runs_active))

    def send_lineage(self, items):
        self._maybe_fail()
        self.sent.append(("lineage", len(items)))


def events_payload(*seqs):
    return [failed_event(seq=s).to_dict() for s in seqs]


class OutboxTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "outbox"

    def test_batches_are_files_named_by_a_growing_zero_padded_counter(self):
        box = Outbox(self.root)
        paths = [box.enqueue("s1", "events", events_payload(i)) for i in (1, 2, 3)]
        self.assertEqual([p.name for p in paths], ["000000000001.json", "000000000002.json", "000000000003.json"])
        self.assertEqual(sorted(os.listdir(self.root / "s1")), [p.name for p in paths])
        doc = json.loads(paths[0].read_text(encoding="utf-8"))
        self.assertEqual(set(doc), {"kind", "payload"})
        self.assertEqual((doc["kind"], doc["payload"][0]["seq"]), ("events", 1))

    def test_each_sink_has_its_own_directory(self):
        box = Outbox(self.root)
        box.enqueue("a", "events", events_payload(1))
        box.enqueue("b", "events", events_payload(1))
        self.assertEqual(sorted(os.listdir(self.root)), ["a", "b"])
        self.assertEqual((box.count("a"), box.count("b")), (1, 1))

    def test_counter_continues_after_a_restart(self):
        Outbox(self.root).enqueue("s1", "events", events_payload(1))
        Outbox(self.root).enqueue("s1", "events", events_payload(2))
        self.assertEqual(sorted(os.listdir(self.root / "s1")), ["000000000001.json", "000000000002.json"])

    def test_no_temporary_files_are_left_behind(self):
        box = Outbox(self.root)
        for i in range(5):
            box.enqueue("s1", "events", events_payload(i))
        self.assertTrue(all(n.endswith(".json") for n in os.listdir(self.root / "s1")))

    def test_a_failed_write_leaves_the_old_file_and_no_temporary_file(self):
        target = self.root / "x.json"
        self.root.mkdir(parents=True)
        atomic_write(target, b"old")
        with mock.patch.object(outbox_module.os, "replace", side_effect=OSError("disk")):
            with self.assertRaises(OSError):
                atomic_write(target, b"new")
        self.assertEqual(target.read_bytes(), b"old")
        self.assertEqual(os.listdir(self.root), ["x.json"])

    def test_rejects_names_and_kinds_that_could_escape_the_directory(self):
        box = Outbox(self.root)
        for bad in ("../x", "A", "a/b", "", "a" * 40):
            with self.assertRaises(ValueError):
                box.enqueue(bad, "events", [])
        with self.assertRaises(ValueError):
            box.enqueue("s1", "other", [])

    def test_oldest_are_dropped_beyond_the_cap_and_counted_across_restarts(self):
        box = Outbox(self.root, cap=5)
        for i in range(1, 9):
            box.enqueue("s1", "events", events_payload(i))
        names = [p.name for p in box.pending("s1")]
        self.assertEqual(names, [f"{n:012d}.json" for n in range(4, 9)])
        self.assertEqual(box.dropped("s1"), 3)
        again = Outbox(self.root, cap=5)
        self.assertEqual(again.dropped("s1"), 3)
        again.enqueue("s1", "events", events_payload(9))
        self.assertEqual(again.dropped("s1"), 4)
        self.assertEqual(again.count("s1"), 5)

    def test_the_default_cap_is_one_thousand(self):
        self.assertEqual(outbox_module.MAX_PENDING, 1000)
        self.assertEqual(Outbox(self.root).cap, 1000)

    # ---- draining ----

    def test_drain_sends_oldest_first_and_deletes_after_success(self):
        box = Outbox(self.root)
        sink = ScriptedSink()
        box.enqueue("s1", "events", events_payload(1, 2))
        box.enqueue("s1", "snapshot", sample_snapshot().to_dict())
        box.enqueue("s1", "lineage", [{"eventType": "START"}])
        box.enqueue("s1", "events", events_payload(3))
        result = box.drain(sink)
        self.assertEqual((result.delivered, result.error, result.pending), (4, None, 0))
        self.assertEqual(sink.sent, [("events", [1, 2]), ("snapshot", 2), ("lineage", 1), ("events", [3])])
        self.assertEqual(box.drain(sink).delivered, 0)
        self.assertEqual(len(sink.sent), 4)

    def test_drain_stops_at_the_first_failure_and_keeps_order(self):
        box = Outbox(self.root)
        sink = ScriptedSink()
        for i in (1, 2, 3):
            box.enqueue("s1", "events", events_payload(i))
        sink.fail = DeliveryError('delivery to sink "s1" failed: HTTP 503', status=503, sink="s1")
        result = box.drain(sink)
        self.assertEqual((result.delivered, result.pending), (0, 3))
        self.assertIn("HTTP 503", result.error)
        self.assertEqual(sink.sent, [])
        sink.fail = None
        result = box.drain(sink)
        self.assertEqual((result.delivered, result.pending, result.error), (3, 0, None))
        self.assertEqual(sink.sent, [("events", [1]), ("events", [2]), ("events", [3])])

    def test_a_failure_in_the_middle_keeps_the_rest_for_later(self):
        box = Outbox(self.root)
        sink = ScriptedSink()
        for i in (1, 2, 3):
            box.enqueue("s1", "events", events_payload(i))
        original = sink.send_events

        def flaky(events):
            if events[0].seq == 2:
                raise DeliveryError("down", sink="s1")
            original(events)

        sink.send_events = flaky
        result = box.drain(sink)
        self.assertEqual((result.delivered, result.pending), (1, 2))
        sink.send_events = original
        box.drain(sink)
        self.assertEqual(sink.sent, [("events", [1]), ("events", [2]), ("events", [3])])

    def test_partial_progress_rewrites_the_batch_without_the_delivered_items(self):
        box = Outbox(self.root)
        sink = ScriptedSink()
        box.enqueue("s1", "events", events_payload(1, 2, 3))
        err = DeliveryError("stopped at the second item", sink="s1", delivered=1)
        sink.fail = err
        result = box.drain(sink)
        self.assertEqual(result.pending, 1)
        (path,) = box.pending("s1")
        self.assertEqual([e["seq"] for e in json.loads(path.read_text(encoding="utf-8"))["payload"]], [2, 3])
        sink.fail = None
        box.drain(sink)
        self.assertEqual(sink.sent, [("events", [2, 3])])

    def test_a_sink_bug_is_reported_by_type_only_and_never_stops_the_caller(self):
        box = Outbox(self.root)
        sink = ScriptedSink()
        box.enqueue("s1", "events", events_payload(1))
        secret = canary()
        sink.fail = RuntimeError("boom " + secret)
        with self.assertLogs("ogun_export.outbox", "WARNING") as logs:
            result = box.drain(sink)
        self.assertEqual(result.pending, 1)
        self.assertIn("RuntimeError", result.error)
        self.assertNotIn(secret, result.error)
        self.assertNotIn(secret, "\n".join(logs.output))

    def test_unreadable_files_are_set_aside_and_later_files_still_go(self):
        box = Outbox(self.root)
        sink = ScriptedSink()
        box.enqueue("s1", "events", events_payload(1))
        (box.pending("s1")[0]).write_text("{not json", encoding="utf-8")
        box.enqueue("s1", "events", events_payload(2))
        with self.assertLogs("ogun_export.outbox", "WARNING"):
            result = box.drain(sink)
        self.assertEqual(result.delivered, 1)
        self.assertEqual(sink.sent, [("events", [2])])
        self.assertEqual([n for n in os.listdir(self.root / "s1") if n.endswith(".bad")], ["000000000001.bad"])

    def test_batches_a_sink_cannot_take_are_discarded(self):
        box = Outbox(self.root)
        sink = ScriptedSink(capabilities={"events"})
        box.enqueue("s1", "snapshot", sample_snapshot().to_dict())
        box.enqueue("s1", "events", events_payload(1))
        with self.assertLogs("ogun_export.outbox", "WARNING"):
            result = box.drain(sink)
        self.assertEqual(sink.sent, [("events", [1])])
        self.assertEqual(result.pending, 0)


class OutboxWithAServerTests(unittest.TestCase):
    """Server down, files accumulate in order, server up, delivered once and in order."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.server = FakeServer().start()
        self.addCleanup(self.server.stop)
        self.box = Outbox(Path(self.tmp.name) / "outbox")
        self.sink = REGISTRY["webhook"].from_config({"name": "hook", "type": "webhook", "url": self.server.url + "/in"}, {})
        self.sink.sleep = noop_sleep

    def delivered_seqs(self):
        return [item["seq"] for r in self.server.requests if r.json()["type"] == "events" for item in r.json()["items"]]

    def test_down_then_up(self):
        self.server.set_default(503)
        for i in (1, 2, 3):
            self.box.enqueue("hook", "events", events_payload(i))
            result = self.box.drain(self.sink)
            self.assertEqual(result.pending, i)
            self.assertIn("HTTP 503", result.error)
        names = [p.name for p in self.box.pending("hook")]
        self.assertEqual(names, sorted(names))
        self.assertEqual(len(names), 3)
        # While down, only the oldest batch is ever attempted, so order cannot be lost.
        self.assertEqual({r.json()["items"][0]["seq"] for r in self.server.requests}, {1})

        self.server.requests.clear()
        self.server.set_default(200)
        result = self.box.drain(self.sink)
        self.assertEqual((result.delivered, result.pending, result.error), (3, 0, None))
        self.assertEqual(self.delivered_seqs(), [1, 2, 3])
        self.assertEqual(self.box.drain(self.sink).delivered, 0)
        self.assertEqual(self.delivered_seqs(), [1, 2, 3])

    def test_a_new_process_finds_the_files_and_delivers_them_once(self):
        self.server.set_default(500)
        self.box.enqueue("hook", "events", events_payload(7))
        self.box.enqueue("hook", "events", events_payload(8))
        self.box.drain(self.sink)
        self.server.requests.clear()
        self.server.set_default(200)
        restarted = Outbox(self.box.root)
        restarted.drain(self.sink)
        restarted.drain(self.sink)
        self.assertEqual(self.delivered_seqs(), [7, 8])


if __name__ == "__main__":
    unittest.main()
