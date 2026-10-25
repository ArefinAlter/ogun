import calendar
import json
import unittest

from fakes import AT, failed_event, run_created, transition
from ogun_export import mapping
from ogun_export.model import ExportEvent, Snapshot, format_time, parse_time_ns


class TimeTests(unittest.TestCase):
    def test_parses_rfc3339_with_milliseconds_and_offsets(self):
        base = calendar.timegm((2026, 10, 24, 12, 0, 0)) * 1_000_000_000
        self.assertEqual(parse_time_ns("2026-10-24T12:00:00Z"), base)
        self.assertEqual(parse_time_ns("2026-10-24T12:00:00.123Z"), base + 123_000_000)
        self.assertEqual(parse_time_ns("2026-10-24T14:00:00.5+02:00"), base + 500_000_000)
        self.assertEqual(parse_time_ns("2026-10-24T09:30:00-02:30"), base)

    def test_rejects_what_is_not_a_timestamp(self):
        for bad in (None, 5, "", "yesterday", "2026-13-01T00:00:00Z", "2026-10-24"):
            self.assertIsNone(parse_time_ns(bad), bad)

    def test_format_is_utc_with_milliseconds(self):
        self.assertEqual(format_time(1_700_000_000.1239), "2023-11-14T22:13:20.123Z")


class ModelTests(unittest.TestCase):
    def test_event_round_trips_through_json(self):
        ev = failed_event()
        again = ExportEvent.from_dict(json.loads(json.dumps(ev.to_dict())))
        self.assertEqual(again, ev)

    def test_event_from_junk_does_not_raise(self):
        for junk in (None, 5, "x", [], {}, {"tags": 5, "seq": "7", "severity": "loud", "run_id": {"a": 1}}):
            ev = ExportEvent.from_dict(junk)
            self.assertEqual(ev.severity, "info")
            self.assertIsInstance(ev.tags, dict)
            self.assertIsNone(ev.seq)

    def test_event_time_helpers(self):
        ev = failed_event(at="2026-10-24T12:00:00.123Z")
        self.assertEqual(ev.unix_s(), calendar.timegm((2026, 10, 24, 12, 0, 0)))
        self.assertEqual(ev.unix_ms(), ev.unix_s() * 1000 + 123)
        self.assertEqual(ev.unix_ns(), ev.unix_ms() * 1_000_000)

    def test_snapshot_round_trips_and_lists_gauges(self):
        snap = Snapshot(
            taken_at="2026-10-24T12:01:00.000Z",
            workflows=3,
            runs_active=2,
            approvals_pending=1,
            streams=4,
            node_instances_by_state={"failed": 1, "running": 2},
            profile="edge",
        )
        self.assertEqual(Snapshot.from_dict(json.loads(json.dumps(snap.to_dict()))), snap)
        gauges = snap.gauges()
        self.assertEqual(
            [g[0] for g in gauges],
            ["ogun.workflows", "ogun.runs.active", "ogun.approvals.pending", "ogun.streams", "ogun.node_instances", "ogun.node_instances"],
        )
        self.assertEqual([g[2] for g in gauges[4:]], [{"state": "running"}, {"state": "failed"}])
        self.assertEqual([g[1] for g in gauges], [3, 2, 1, 4, 2, 1])

    def test_snapshot_from_junk(self):
        snap = Snapshot.from_dict({"workflows": "x", "node_instances_by_state": {"running": "many", "failed": 2}})
        self.assertEqual(snap.workflows, 0)
        self.assertEqual(snap.node_instances_by_state, {"failed": 2})


class MappingTests(unittest.TestCase):
    def test_transition_to_failed_is_an_error(self):
        ev = mapping.map_event(transition(12, "orun_1", "ni_1", "failed", cause="http.timeout"))
        self.assertEqual((ev.kind, ev.severity), ("node_transition", "error"))
        self.assertEqual((ev.seq, ev.run_id, ev.instance_id), (12, "orun_1", "ni_1"))
        self.assertEqual((ev.from_state, ev.to_state, ev.cause), ("running", "failed", "http.timeout"))
        self.assertEqual(ev.at, AT)
        self.assertIn("failed", ev.title)
        self.assertIn("http.timeout", ev.text)
        self.assertEqual(ev.dedup_key, "orun_1:ni_1:-")

    def test_retry_scheduled_is_a_warning(self):
        ev = mapping.map_event(transition(1, "orun_1", "ni_1", "retry_scheduled", cause="http.503"))
        self.assertEqual(ev.severity, "warning")
        self.assertIn("retried", ev.title)

    def test_waiting_for_user_is_attention_and_says_a_person_must_act(self):
        ev = mapping.map_event(transition(1, "orun_1", "ni_1", "waiting_for_user", frm="ready"))
        self.assertEqual(ev.severity, "attention")
        self.assertIn("waiting for a person", ev.title)
        self.assertIn("Ogun CLI", ev.text)

    def test_everything_else_is_info(self):
        for state in ("pending", "ready", "running", "waiting", "waiting_for_session", "succeeded", "cancelled", "skipped", "never_heard_of_it"):
            with self.subTest(state=state):
                self.assertEqual(mapping.map_event(transition(1, "orun_1", "ni_1", state)).severity, "info")

    def test_run_level_transition_has_no_instance(self):
        ev = mapping.map_event(transition(5, "orun_1", None, "failed", frm="running"))
        self.assertEqual((ev.kind, ev.severity, ev.dedup_key), ("run_transition", "error", "orun_1"))

    def test_run_created(self):
        ev = mapping.map_event(run_created(2, "orun_9", instances=4))
        self.assertEqual((ev.kind, ev.severity), ("run_created", "info"))
        self.assertIn("4 node instances", ev.text)

    def test_unknown_kind_becomes_a_generic_event(self):
        ev = mapping.map_event({"seq": 3, "at": AT, "run_id": "orun_1", "kind": {"type": "brand_new", "x": 1}, "extra": [1, 2]})
        self.assertEqual((ev.kind, ev.severity, ev.seq), ("event", "info", 3))
        self.assertEqual(ev.tags["daemon_event_type"], "brand_new")

    def test_extra_keys_are_ignored(self):
        raw = transition(4, "orun_1", "ni_1", "failed")
        raw["surprise"] = {"nested": True}
        raw["kind"]["surprise"] = 1
        self.assertEqual(mapping.map_event(raw).severity, "error")

    def test_never_raises_on_odd_input(self):
        odd = [
            {},
            {"kind": None},
            {"kind": []},
            {"kind": "transition"},
            {"kind": {"type": None}},
            {"kind": {"type": ["transition"]}},
            {"kind": {"type": "transition"}},
            {"kind": {"type": "transition", "to": {"deep": 1}, "from": 5, "cause": ["x"]}},
            {"seq": "7", "at": 12345, "run_id": 99, "instance_id": {"a": 1}, "kind": {"type": "run_created", "instances": "four"}},
            {"seq": -1, "at": "not a time", "kind": {"type": "transition", "to": "failed\n\x00bad"}},
            {"seq": True, "kind": 5},
            {"run_id": "x" * 10000, "kind": {"type": "t" * 10000}},
        ]
        for raw in odd:
            with self.subTest(raw=str(raw)[:60]):
                ev = mapping.map_event(raw)
                self.assertIsInstance(ev, ExportEvent)
                json.dumps(ev.to_dict())
                self.assertLessEqual(len(ev.title), 400)
                self.assertNotIn("\n", ev.title)

    def test_non_objects_map_to_none(self):
        for raw in (None, 5, "x", [], True):
            self.assertIsNone(mapping.map_event(raw))

    def test_bad_timestamp_falls_back_to_now(self):
        ev = mapping.map_event({"seq": 1, "at": "soon", "kind": {"type": "x"}}, now=1_700_000_000.0)
        self.assertEqual(ev.at, "2023-11-14T22:13:20.000Z")

    def test_control_characters_are_flattened(self):
        ev = mapping.map_event(transition(1, "orun_1", "ni_1", "failed", cause="line1\nline2\rline3"))
        self.assertEqual(ev.cause, "line1 line2 line3")

    def test_enrich_names_workflow_node_and_partition_and_rekeys_the_alert(self):
        ev = mapping.map_event(transition(1, "orun_1", "ni_1", "failed"))
        mapping.enrich(ev, "noaa-daily", "fetch", "2026-01-05")
        self.assertEqual(ev.dedup_key, "orun_1:fetch:2026-01-05")
        self.assertIn("noaa-daily/fetch", ev.title)
        self.assertEqual(ev.tags["workflow"], "noaa-daily")

    def test_run_finished_event(self):
        view = {
            "run_id": "orun_1",
            "workflow": "noaa-daily",
            "state": "failed",
            "partition_count": 2,
            "nodes": [{"state": "succeeded"}, {"state": "failed"}, {"state": "succeeded"}],
        }
        ev = mapping.run_finished_event(view, "orun_1", AT, 9)
        self.assertEqual((ev.kind, ev.severity, ev.to_state, ev.seq), ("run_finished", "error", "failed", 9))
        self.assertEqual(ev.title, "noaa-daily run orun_1 failed")
        self.assertIn("2 partitions", ev.text)
        self.assertIn("failed 1", ev.text)
        for state, severity in (("succeeded", "info"), ("cancelled", "info")):
            self.assertEqual(mapping.run_finished_event({"state": state}, "orun_2", AT, None).severity, severity)

    def test_recovered_event_keeps_the_original_key(self):
        ev = mapping.map_event(transition(8, "orun_1", "ni_1", "succeeded"))
        out = mapping.recovered_event(ev, "orun_1:fetch:2026-01-05")
        self.assertEqual((out.kind, out.severity, out.dedup_key), ("node_recovered", "info", "orun_1:fetch:2026-01-05"))
        self.assertIn("recovered", out.title)

    def test_approval_events(self):
        a = {"approval_id": "appr_1", "kind": "egress", "state": "pending", "workflow": "noaa-daily", "node": "fetch", "requested_by": "agent:export"}
        ev = mapping.approval_pending_event(a, AT)
        self.assertEqual((ev.kind, ev.severity, ev.dedup_key), ("approval_pending", "attention", "appr_1"))
        self.assertIn("waiting for a person", ev.title)
        self.assertIsNone(mapping.approval_pending_event({"kind": "egress"}, AT))
        done = mapping.approval_resolved_event("appr_1", {"workflow": "noaa-daily"}, AT)
        self.assertEqual((done.kind, done.severity, done.dedup_key), ("approval_resolved", "info", "appr_1"))


if __name__ == "__main__":
    unittest.main()
