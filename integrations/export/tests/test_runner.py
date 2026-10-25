import json
import tempfile
import threading
import unittest
from pathlib import Path

from fakes import (
    LINEAGE,
    FakeServer,
    StubClient,
    StubError,
    canary,
    node_view,
    run_created,
    run_view,
    transition,
)
from ogun_export import runner as runner_module
from ogun_export.runner import Exporter, ReconnectingClient, is_transient
from ogun_export.sinks import REGISTRY

READ_ONLY = {"status", "list_runs", "get_run", "events_since", "list_approvals", "lineage"}


class RunnerCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.state = Path(self.tmp.name) / "state"
        self.server = FakeServer().start()
        self.addCleanup(self.server.stop)
        self.client = StubClient()
        self.now = [1_700_000_000.0]
        self.sleeps = []

    # ---- helpers ----

    def webhook(self, name="hook", server=None, **extra):
        cfg = {"name": name, "type": "webhook", "url": (server or self.server).url + "/in"}
        cfg.update(extra)
        return REGISTRY["webhook"].from_config(cfg, {})

    def pagerduty(self, name="pd"):
        sink = REGISTRY["pagerduty"].from_config({"name": name, "type": "pagerduty", "routing_key": {"env": "K"}}, {"K": "rk"})
        sink.enqueue_url = self.server.url + "/pd"
        return sink

    def exporter(self, *sinks, **kw):
        return Exporter(self.client, list(sinks), self.state, clock=lambda: self.now[0], sleep=self.sleeps.append, **kw)

    def start_at(self, seq):
        self.state.mkdir(parents=True, exist_ok=True)
        (self.state / "cursor.json").write_text(json.dumps({"after": seq}), encoding="utf-8")

    def cursor(self):
        return json.loads((self.state / "cursor.json").read_text(encoding="utf-8"))["after"]

    def bodies(self, type_, server=None):
        return [r.json() for r in (server or self.server).requests if r.route == "/in" and r.json()["type"] == type_]

    def items(self, server=None):
        return [item for body in self.bodies("events", server) for item in body["items"]]

    def populate(self, run_state="running", nodes=None):
        nodes = nodes or [node_view("ni_1", "fetch", "2026-01-05", "failed"), node_view("ni_2", "fetch", "2026-01-06", "running")]
        self.client.runs["orun_1"] = run_view("orun_1", "noaa-daily", run_state, nodes)


class CursorTests(RunnerCase):
    def test_first_start_begins_at_the_daemon_s_last_event_seq_not_at_zero(self):
        self.populate()
        self.client.add(*[transition(i, "orun_1", "ni_1", "failed") for i in range(1, 6)])
        exp = self.exporter(self.webhook())
        exp.run_once()
        self.assertEqual(self.client.events_since_args[0]["after"], 5)
        self.assertEqual(self.items(), [])
        self.assertEqual(self.cursor(), 5)
        self.client.add(transition(6, "orun_1", "ni_1", "failed"))
        exp.run_once()
        self.assertEqual([i["seq"] for i in self.items()], [6])
        self.assertEqual(self.cursor(), 6)

    def test_a_second_start_uses_the_stored_cursor(self):
        self.populate()
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        self.start_at(0)
        self.exporter(self.webhook()).run_once()
        self.assertEqual([i["seq"] for i in self.items()], [1])

    def test_the_cursor_is_persisted_only_after_the_batch_is_in_the_outbox(self):
        self.populate()
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        exp = self.exporter(self.webhook())
        exp.run_once()
        self.client.add(transition(2, "orun_1", "ni_1", "failed"))
        seen = []
        original = exp._write_cursor

        def spy(after):
            seen.append((after, exp.outbox.count("hook")))
            original(after)

        exp._write_cursor = spy
        self.server.set_default(503)
        exp.run_once()
        self.assertEqual(seen, [(2, 1)])
        self.assertEqual(self.cursor(), 2)

    def test_a_crash_before_the_cursor_is_saved_resends_and_never_loses(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        first = self.exporter(self.webhook())

        def crash(after):
            raise RuntimeError("power cut")

        first._write_cursor = crash
        summary = first.run_once()
        self.assertEqual(summary["daemon_error"], "RuntimeError")
        self.assertEqual(self.cursor(), 0)
        second = self.exporter(self.webhook())
        second.run_once()
        seqs = [i["seq"] for i in self.items()]
        self.assertIn(1, seqs)
        self.assertLessEqual(seqs.count(1), 2)
        self.assertEqual(self.cursor(), 1)

    def test_restart_with_a_sink_down_loses_nothing_and_delivers_each_event_once(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"), transition(2, "orun_1", "ni_2", "failed"))
        self.server.set_default(503)
        down = self.exporter(self.webhook())
        summary = down.run_once()
        self.assertIn("hook", summary["sink_errors"])
        self.assertEqual(self.cursor(), 2)
        self.assertGreaterEqual(down.outbox.count("hook"), 1)
        self.server.requests.clear()
        self.server.set_default(200)
        up = self.exporter(self.webhook())
        summary = up.run_once()
        self.assertEqual(summary["sink_errors"], {})
        self.assertEqual(sorted(i["seq"] for i in self.items()), [1, 2])
        up.run_once()
        self.assertEqual(sorted(i["seq"] for i in self.items()), [1, 2])
        self.assertEqual(up.outbox.count("hook"), 0)

    def test_events_at_or_below_the_cursor_are_ignored(self):
        self.populate()
        self.start_at(5)
        self.client.add(transition(3, "orun_1", "ni_1", "failed"), transition(5, "orun_1", "ni_1", "failed"), transition(6, "orun_1", "ni_1", "failed"))
        self.exporter(self.webhook()).run_once()
        self.assertEqual([i["seq"] for i in self.items()], [6])

    def test_a_long_backlog_is_read_in_pages(self):
        self.start_at(0)
        self.client.add(*[{"seq": i, "at": "2026-10-24T12:00:00.000Z", "kind": {"type": "something_new"}} for i in range(1, 2501)])
        exp = self.exporter(self.webhook())
        summary = exp.run_once()
        self.assertEqual(summary["events_seen"], 2500)
        self.assertEqual(self.cursor(), 2500)
        self.assertEqual([a["after"] for a in self.client.events_since_args], [0, 1000, 2000])
        self.assertEqual([a["limit"] for a in self.client.events_since_args], [1000, 1000, 1000])

    def test_run_once_does_not_wait_and_the_loop_long_polls(self):
        self.start_at(0)
        exp = self.exporter(self.webhook())
        exp.run_once()
        self.assertEqual(self.client.events_since_args[0]["wait_ms"], 0)
        stop = threading.Event()
        calls = []

        def hook():
            calls.append(1)
            if len(calls) >= 2:
                stop.set()

        self.client.on_events_since = hook
        self.client.events_since_args.clear()
        exp.run_forever(stop)
        self.assertEqual({a["wait_ms"] for a in self.client.events_since_args}, {15000})
        long_poll = self.exporter(self.webhook(name="other"), interval_s=60)
        self.client.events_since_args.clear()
        stop.clear()
        calls.clear()
        long_poll.run_forever(stop)
        self.assertEqual({a["wait_ms"] for a in self.client.events_since_args}, {25000})


class MappingInTheRunnerTests(RunnerCase):
    def test_failed_node_is_enriched_and_delivered_by_default(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed", cause="http.timeout"))
        self.exporter(self.webhook()).run_once()
        (item,) = self.items()
        self.assertEqual((item["severity"], item["kind"]), ("error", "node_transition"))
        self.assertEqual((item["workflow"], item["node"], item["partition"]), ("noaa-daily", "fetch", "2026-01-05"))
        self.assertEqual(item["dedup_key"], "orun_1:fetch:2026-01-05")
        self.assertIn("noaa-daily/fetch", item["title"])

    def test_info_events_are_not_sent_unless_asked_for(self):
        self.populate()
        self.start_at(0)
        self.client.add(run_created(1, "orun_1"), transition(2, "orun_1", "ni_1", "running", frm="ready"))
        self.exporter(self.webhook()).run_once()
        self.assertEqual(self.items(), [])
        # Only the snapshot looked at the run; no info event was worth a get_run of its own.
        self.assertEqual(self.client.calls.count("get_run"), 1)
        self.start_at(0)
        self.exporter(self.webhook(name="all", min_severity="info")).run_once()
        self.assertEqual([i["kind"] for i in self.items()], ["run_created", "node_transition"])

    def test_retry_scheduled_is_a_warning_and_passes_the_default(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "retry_scheduled", cause="http.503"))
        self.exporter(self.webhook()).run_once()
        self.assertEqual([i["severity"] for i in self.items()], ["warning"])

    def test_waiting_for_user_is_attention(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_2", "waiting_for_user", frm="ready"))
        self.exporter(self.webhook(min_severity="error")).run_once()
        (item,) = self.items()
        self.assertEqual(item["severity"], "attention")
        self.assertIn("waiting for a person", item["title"])

    def test_unknown_and_malformed_events_never_stop_the_exporter(self):
        self.populate()
        self.start_at(0)
        self.client.add(
            "junk",
            None,
            5,
            [],
            {"seq": 1, "kind": 7},
            {"seq": "two", "kind": {"type": "transition", "to": {"x": 1}}},
            {"seq": 3, "at": "soon", "run_id": 9, "kind": {"type": "brand_new", "payload": {"deep": [1, 2]}}},
            transition(4, "orun_1", "ni_1", "failed"),
        )
        exp = self.exporter(self.webhook(min_severity="info"))
        summary = exp.run_once()
        self.assertIsNone(summary["daemon_error"])
        self.assertEqual(self.cursor(), 4)
        self.assertEqual(sorted(i["seq"] for i in self.items() if i["seq"] is not None), [1, 3, 4])

    def test_a_run_the_daemon_no_longer_knows_still_exports_the_event(self):
        self.start_at(0)
        self.client.add(transition(1, "orun_gone", "ni_9", "failed"))
        self.exporter(self.webhook()).run_once()
        (item,) = self.items()
        self.assertIsNone(item["workflow"])
        self.assertEqual(item["dedup_key"], "orun_gone:ni_9:-")

    def test_an_unreachable_daemon_during_enrichment_loses_nothing(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        original = self.client.get_run

        def down(run_id):
            raise OSError("socket closed")

        self.client.get_run = down
        exp = self.exporter(self.webhook())
        summary = exp.run_once()
        self.assertEqual(summary["daemon_error"], "OSError")
        self.assertEqual(self.cursor(), 0)
        self.assertEqual(self.items(), [])
        self.client.get_run = original
        self.assertIsNone(exp.run_once()["daemon_error"])
        self.assertEqual([i["seq"] for i in self.items()], [1])


class RunFinishedTests(RunnerCase):
    def finished_run(self, state="succeeded"):
        node_state = "failed" if state == "failed" else "succeeded"
        self.populate(state, [node_view("ni_1", "fetch", "d1", node_state), node_view("ni_2", "fetch", "d2", "succeeded")])
        self.client.lineages["orun_1"] = LINEAGE

    def test_run_finished_is_sent_once_and_lineage_is_forwarded_once(self):
        self.finished_run()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "succeeded"), transition(2, "orun_1", "ni_2", "succeeded"))
        exp = self.exporter(self.webhook())
        exp.run_once()
        (item,) = self.items()
        self.assertEqual((item["kind"], item["severity"], item["to_state"]), ("run_finished", "info", "succeeded"))
        self.assertEqual(item["title"], "noaa-daily run orun_1 succeeded")
        lineage = self.bodies("lineage")
        self.assertEqual(len(lineage), 1)
        self.assertEqual(lineage[0]["items"], LINEAGE)
        self.assertEqual(self.client.calls.count("lineage"), 1)
        ledger = json.loads((self.state / "lineage-sent.json").read_text(encoding="utf-8"))
        self.assertEqual(ledger["run_ids"], ["orun_1"])
        self.assertEqual(ledger["pending"], [])
        self.client.add(transition(3, "orun_1", "ni_2", "succeeded"), transition(4, "orun_1", None, "succeeded", frm="running"))
        exp.run_once()
        self.assertEqual(len(self.items()), 1)
        self.assertEqual(len(self.bodies("lineage")), 1)
        restarted = self.exporter(self.webhook())
        self.client.add(transition(5, "orun_1", "ni_1", "succeeded"))
        restarted.run_once()
        self.assertEqual(len(self.items()), 1)
        self.assertEqual(len(self.bodies("lineage")), 1)
        self.assertEqual(self.client.calls.count("lineage"), 1)

    def test_a_failed_run_is_an_error(self):
        self.finished_run("failed")
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        self.exporter(self.webhook(min_severity="error")).run_once()
        self.assertEqual([(i["kind"], i["severity"]) for i in self.items()], [("node_transition", "error"), ("run_finished", "error")])

    def test_a_cancelled_run_is_info_and_still_passes_the_default_filter(self):
        self.finished_run("cancelled")
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "cancelled"))
        self.exporter(self.webhook()).run_once()
        (item,) = self.items()
        self.assertEqual((item["kind"], item["severity"], item["to_state"]), ("run_finished", "info", "cancelled"))

    def test_the_run_level_transition_is_folded_into_run_finished(self):
        self.finished_run("failed")
        self.start_at(0)
        self.client.add(transition(1, "orun_1", None, "failed", frm="running"))
        self.exporter(self.webhook()).run_once()
        self.assertEqual([i["kind"] for i in self.items()], ["run_finished"])

    def test_a_run_that_has_not_finished_yet_is_not_announced(self):
        self.populate("running", [node_view("ni_1", "fetch", "d1", "succeeded"), node_view("ni_2", "fetch", "d2", "running")])
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "succeeded"))
        exp = self.exporter(self.webhook())
        exp.run_once()
        self.assertEqual(self.items(), [])
        self.client.runs["orun_1"]["state"] = "succeeded"
        self.client.add(transition(2, "orun_1", "ni_2", "succeeded"))
        exp.run_once()
        self.assertEqual([i["kind"] for i in self.items()], ["run_finished"])

    def test_lineage_is_not_fetched_when_no_sink_wants_it(self):
        self.finished_run()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "succeeded"))
        pd = self.pagerduty()
        self.exporter(pd).run_once()
        self.assertNotIn("lineage", self.client.calls)

    def test_a_lineage_fetch_that_fails_for_the_daemon_being_down_is_retried(self):
        self.finished_run()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "succeeded"))
        original = self.client.lineage

        def down(run_id=None, blake3_digest=None):
            raise OSError("closed")

        self.client.lineage = down
        exp = self.exporter(self.webhook())
        summary = exp.run_once()
        self.assertEqual(summary["daemon_error"], "OSError")
        self.assertEqual(len(self.items()), 1)
        self.assertEqual(self.bodies("lineage"), [])
        self.client.lineage = original
        exp.run_once()
        self.assertEqual(len(self.bodies("lineage")), 1)
        exp.run_once()
        self.assertEqual(len(self.bodies("lineage")), 1)
        self.assertEqual(len(self.items()), 1)

    def test_lineage_for_a_run_the_daemon_cannot_describe_is_skipped_not_retried_forever(self):
        self.finished_run()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "succeeded"))

        def refuse(run_id=None, blake3_digest=None):
            raise StubError("lineage.not_found")

        self.client.lineage = refuse
        exp = self.exporter(self.webhook())
        self.assertIsNone(exp.run_once()["daemon_error"])
        ledger = json.loads((self.state / "lineage-sent.json").read_text(encoding="utf-8"))
        self.assertEqual(ledger["pending"], [])


class PagerDutyThroughTheRunnerTests(RunnerCase):
    def pd_bodies(self):
        return [r.json() for r in self.server.requests if r.route == "/pd"]

    def test_trigger_then_resolve_when_the_same_node_instance_succeeds(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        exp = self.exporter(self.pagerduty())
        exp.run_once()
        (trigger,) = self.pd_bodies()
        self.assertEqual((trigger["event_action"], trigger["dedup_key"]), ("trigger", "orun_1:fetch:2026-01-05"))
        self.assertEqual(trigger["payload"]["severity"], "error")
        self.client.add(transition(2, "orun_1", "ni_2", "succeeded"))
        exp.run_once()
        self.assertEqual(len(self.pd_bodies()), 1)
        # A new process resolves too: the open alert is stored on disk.
        again = self.exporter(self.pagerduty())
        self.client.add(transition(3, "orun_1", "ni_1", "succeeded", frm="retry_scheduled"))
        again.run_once()
        trigger, resolve = self.pd_bodies()
        self.assertEqual(resolve, {"routing_key": "rk", "event_action": "resolve", "dedup_key": "orun_1:fetch:2026-01-05"})
        self.client.add(transition(4, "orun_1", "ni_1", "succeeded"))
        again.run_once()
        self.assertEqual(len(self.pd_bodies()), 2)

    def test_waiting_for_user_triggers_and_success_resolves(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_2", "waiting_for_user", frm="ready"))
        exp = self.exporter(self.pagerduty())
        exp.run_once()
        self.client.add(transition(2, "orun_1", "ni_2", "succeeded"))
        exp.run_once()
        trigger, resolve = self.pd_bodies()
        self.assertEqual(trigger["payload"]["severity"], "warning")
        self.assertEqual(resolve["dedup_key"], trigger["dedup_key"])

    def test_no_resolve_for_a_node_that_never_alerted(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "succeeded"))
        self.exporter(self.pagerduty()).run_once()
        self.assertEqual(self.pd_bodies(), [])


class PeriodicTests(RunnerCase):
    def test_snapshot_counts_and_cadence(self):
        self.populate("running", [node_view("ni_1", "fetch", "d1", "running"), node_view("ni_2", "fetch", "d2", "running"), node_view("ni_3", "load", "d1", "failed")])
        self.client.runs["orun_0"] = run_view("orun_0", "noaa-daily", "succeeded", [node_view("ni_0", "fetch", "d0", "succeeded")])
        self.start_at(0)
        exp = self.exporter(self.webhook())
        summary = exp.run_once()
        self.assertTrue(summary["snapshot"])
        (snap,) = self.bodies("snapshot")
        data = snap["snapshot"]
        self.assertEqual((data["workflows"], data["runs_active"], data["approvals_pending"], data["streams"]), (3, 1, 0, 1))
        self.assertEqual(data["taken_at"], "2023-11-14T22:13:20.000Z")
        by_state = data["node_instances_by_state"]
        self.assertEqual((by_state["running"], by_state["failed"], by_state["succeeded"], by_state["pending"]), (2, 1, 0, 0))
        self.assertIsNone(data["profile"])
        self.assertFalse(exp.run_once()["snapshot"])
        self.now[0] += 59
        self.assertFalse(exp.run_once()["snapshot"])
        self.now[0] += 2
        self.assertTrue(exp.run_once()["snapshot"])
        self.assertEqual(len(self.bodies("snapshot")), 2)

    def test_snapshot_carries_the_profile_the_daemon_reports(self):
        self.client.status_overrides["profile"] = "edge"
        self.start_at(0)
        self.exporter(self.webhook()).run_once()
        self.assertEqual(self.bodies("snapshot")[0]["snapshot"]["profile"], "edge")

    def test_no_snapshot_is_taken_when_no_sink_wants_metrics(self):
        self.start_at(0)
        exp = self.exporter(self.pagerduty())
        self.assertFalse(exp.run_once()["snapshot"])
        self.assertNotIn("list_runs", self.client.calls)

    def test_a_metrics_only_exporter_never_looks_up_runs_for_events(self):
        self.populate("succeeded", [node_view("ni_1", "fetch", "d1", "failed")])
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"), transition(2, "orun_1", None, "failed"))
        prom = REGISTRY["prometheus"].from_config({"name": "prom", "type": "prometheus", "url": self.server.url}, {})
        summary = self.exporter(prom).run_once()
        self.assertTrue(summary["snapshot"])
        self.assertEqual(self.client.calls.count("get_run"), 0)
        self.assertEqual(self.client.calls.count("lineage"), 0)
        self.assertEqual(self.cursor(), 2)

    def test_snapshot_interval_is_configurable(self):
        self.start_at(0)
        exp = self.exporter(self.webhook(), snapshot_interval_s=5)
        exp.run_once()
        self.now[0] += 6
        self.assertTrue(exp.run_once()["snapshot"])

    def test_pending_approvals_become_attention_events_once_and_resolve_later(self):
        approval = {
            "approval_id": "appr_1",
            "kind": "egress",
            "state": "pending",
            "workflow": "noaa-daily",
            "node": "fetch",
            "requested_by": "agent:export",
            "created_at": "2026-10-24T11:59:00.000Z",
        }
        self.client.approvals = [approval]
        self.start_at(0)
        pd = self.pagerduty()
        hook = self.webhook(min_severity="error")
        exp = self.exporter(hook, pd)
        summary = exp.run_once()
        self.assertEqual(summary["approval_events"], 1)
        (item,) = self.items()
        self.assertEqual((item["kind"], item["severity"], item["dedup_key"]), ("approval_pending", "attention", "appr_1"))
        self.assertIn("waiting for a person", item["title"])
        self.now[0] += 61
        self.assertEqual(exp.run_once()["approval_events"], 0)
        self.assertEqual(len(self.items()), 1)
        self.client.approvals = []
        self.now[0] += 61
        self.assertEqual(exp.run_once()["approval_events"], 1)
        self.assertEqual(len(self.items()), 1)
        trigger, resolve = [r.json() for r in self.server.requests if r.route == "/pd"]
        self.assertEqual((trigger["event_action"], trigger["dedup_key"]), ("trigger", "appr_1"))
        self.assertEqual((resolve["event_action"], resolve["dedup_key"]), ("resolve", "appr_1"))
        restarted = self.exporter(hook, pd)
        self.now[0] += 61
        self.assertEqual(restarted.run_once()["approval_events"], 0)

    def test_a_restart_does_not_announce_the_same_pending_approval_again(self):
        self.client.approvals = [{"approval_id": "appr_1", "kind": "code", "state": "pending"}]
        self.start_at(0)
        self.exporter(self.webhook()).run_once()
        self.now[0] += 61
        self.exporter(self.webhook()).run_once()
        self.assertEqual(len(self.items()), 1)


class FailureIsolationTests(RunnerCase):
    def test_one_failing_sink_never_blocks_the_others(self):
        bad_server = FakeServer().start()
        self.addCleanup(bad_server.stop)
        bad_server.set_default(503)
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        exp = self.exporter(self.webhook("bad", server=bad_server), self.webhook("good"))
        summary = exp.run_once()
        self.assertEqual(list(summary["sink_errors"]), ["bad"])
        self.assertEqual([i["seq"] for i in self.items()], [1])
        status = exp.status()["sinks"]
        self.assertEqual((status["good"]["pending"], status["good"]["last_error"]), (0, None))
        self.assertGreaterEqual(status["bad"]["pending"], 1)
        self.assertIn("HTTP 503", status["bad"]["last_error"])
        self.assertEqual(status["bad"]["dropped"], 0)

    def test_a_daemon_error_is_reported_without_raising_and_pending_batches_still_go(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        self.server.set_default(503)
        exp = self.exporter(self.webhook())
        exp.run_once()
        self.server.set_default(200)
        self.server.requests.clear()
        secret = canary()
        self.client.fail_events_with = OSError("socket " + secret)
        with self.assertLogs("ogun_export", "WARNING") as logs:
            summary = exp.run_once()
        self.assertEqual(summary["daemon_error"], "OSError")
        self.assertNotIn(secret, "\n".join(logs.output))
        self.assertEqual([i["seq"] for i in self.items()], [1])
        self.client.fail_events_with = None
        self.assertIsNone(exp.run_once()["daemon_error"])

    def test_sink_backoff_is_respected_by_the_loop_but_not_by_run_once(self):
        self.populate()
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"))
        self.server.set_default(503)
        exp = self.exporter(self.webhook())
        exp.run_once(respect_backoff=True)
        attempts = len(self.server.requests)
        self.assertGreater(attempts, 0)
        exp.run_once(respect_backoff=True)
        self.assertEqual(len(self.server.requests), attempts)
        self.now[0] += 16
        exp.run_once(respect_backoff=True)
        self.assertGreater(len(self.server.requests), attempts)
        again = len(self.server.requests)
        exp.run_once()
        self.assertGreater(len(self.server.requests), again)

    def test_sink_names_must_be_unique(self):
        with self.assertRaises(ValueError):
            self.exporter(self.webhook("same"), self.webhook("same"))


class LoopTests(RunnerCase):
    def test_the_loop_stops_when_asked_and_paces_itself_when_idle(self):
        self.start_at(0)
        stop = threading.Event()
        calls = []

        def hook():
            calls.append(1)
            if len(calls) >= 3:
                stop.set()

        self.client.on_events_since = hook
        self.exporter(self.webhook()).run_forever(stop)
        self.assertEqual(len(calls), 3)
        self.assertEqual(self.sleeps, [1.0, 1.0])

    def test_the_loop_backs_off_while_the_daemon_is_unreachable(self):
        self.start_at(0)
        stop = threading.Event()
        calls = []

        def hook():
            calls.append(1)
            if len(calls) >= 4:
                stop.set()

        self.client.on_events_since = hook
        self.client.fail_events_with = OSError("down")
        self.exporter(self.webhook()).run_forever(stop)
        self.assertEqual(len(calls), 4)
        # 2 s, 4 s and 8 s, slept in slices of at most one second so that a stop request is noticed.
        self.assertEqual(sum(self.sleeps), 14.0)
        self.assertLessEqual(max(self.sleeps), 1.0)


class ReadOnlyTests(RunnerCase):
    def test_only_read_only_methods_are_called(self):
        self.populate("succeeded", [node_view("ni_1", "fetch", "d1", "succeeded")])
        self.client.lineages["orun_1"] = LINEAGE
        self.client.approvals = [{"approval_id": "appr_1", "kind": "code", "state": "pending"}]
        self.start_at(0)
        self.client.add(transition(1, "orun_1", "ni_1", "failed"), transition(2, "orun_1", "ni_1", "succeeded"))
        exp = self.exporter(self.webhook(), self.pagerduty())
        exp.run_once()
        self.now[0] += 100
        exp.run_once()
        self.assertTrue(self.client.calls)
        self.assertLessEqual(set(self.client.calls), READ_ONLY)

    def test_the_reconnecting_client_exposes_only_read_only_calls(self):
        public = {n for n in dir(ReconnectingClient) if not n.startswith("_")}
        self.assertEqual(public, READ_ONLY | {"close"})
        for forbidden in ("call", "decide_approval", "approvals_decide", "put_workflow", "start_run", "cancel_run"):
            self.assertFalse(hasattr(ReconnectingClient, forbidden))

    def test_the_reconnecting_client_connects_lazily_and_after_a_dropped_connection(self):
        made = []

        def factory():
            c = StubClient()
            made.append(c)
            return c

        client = ReconnectingClient(factory)
        self.assertEqual(made, [])
        client.status()
        client.status()
        self.assertEqual(len(made), 1)
        made[0].fail_events_with = StubError("api.unavailable")
        with self.assertRaises(StubError):
            client.events_since(after=0)
        self.assertTrue(made[0].closed)
        client.status()
        self.assertEqual(len(made), 2)
        made[1].fail_events_with = StubError("runs.not_found")
        with self.assertRaises(StubError):
            client.events_since(after=0)
        self.assertFalse(made[1].closed)

    def test_transient_errors(self):
        self.assertTrue(is_transient(OSError()))
        self.assertTrue(is_transient(ConnectionResetError()))
        self.assertTrue(is_transient(StubError("api.unavailable")))
        self.assertFalse(is_transient(StubError("runs.not_found")))
        self.assertFalse(is_transient(ValueError()))
        self.assertEqual(runner_module.MAX_PAGES * runner_module.PAGE_SIZE, 100000)


if __name__ == "__main__":
    unittest.main()
