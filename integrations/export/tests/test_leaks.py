"""No secret value (a canary string) may appear in an exception, a repr, a log record, an outbox
file, a status report or the output of ``check``."""

import io
import json
import logging
import tempfile
import unittest
from pathlib import Path

from fakes import LINEAGE, FakeServer, StubClient, attention_event, failed_event, node_view, noop_sleep, run_view, sample_snapshot, transition
from ogun_export.cli import main
from ogun_export.http import DeliveryError
from ogun_export.runner import Exporter
from ogun_export.secrets import env_names
from ogun_export.sinks import REGISTRY
from test_sinks import build, make_cases


def forbidden_values(case, env):
    """Every secret a case holds: the canary parts and each whole environment value."""
    return set(case.secrets) | {v for v in env.values()}


def echo(req):
    """A server that answers 500 and repeats everything it was sent, secrets included."""
    body = {"path": req.path, "headers": req.headers, "body": req.body.decode("utf-8", "replace")}
    return 500, json.dumps(body).encode(), {}


def send_everything(sink):
    for name, send in (
        ("events", lambda: sink.send_events([failed_event(), attention_event()])),
        ("metrics", lambda: sink.send_snapshot(sample_snapshot())),
        ("lineage", lambda: sink.send_lineage(LINEAGE)),
    ):
        if name in sink.capabilities:
            yield name, send


class SinkLeakTests(unittest.TestCase):
    def test_delivery_errors_and_reprs_never_contain_a_secret(self):
        for case in make_cases():
            with self.subTest(sink=case.type), FakeServer(echo) as server:
                sink, env = build(case, server)
                secrets = forbidden_values(case, env)
                self.assertTrue(secrets)
                texts = [repr(sink), str(sink), repr(vars(sink)), repr(sink.__dict__)]
                for name, send in send_everything(sink):
                    with self.assertRaises(DeliveryError) as ctx:
                        send()
                    e = ctx.exception
                    texts += [str(e), repr(e), repr(e.args), str(e.status), e.sink]
                    self.assertIsNone(e.__cause__)
                    self.assertIsNone(e.__context__)
                    self.assertIn(f'"{sink.name}"', str(e))
                    self.assertIn("HTTP 500", str(e))
                    self.assertNotIn("?", str(e))
                    self.assertNotIn("@", str(e))
                self.assertTrue(server.requests)
                blob = "\n".join(texts)
                for secret in secrets:
                    self.assertNotIn(secret, blob, f"{case.type} leaked a secret")

    def test_replies_that_echo_a_secret_in_a_failure_code_do_not_leak_it(self):
        for case in make_cases():
            if case.type not in ("dingtalk", "feishu", "wecom", "splunk", "elastic"):
                continue
            with self.subTest(sink=case.type), FakeServer() as server:
                sink, env = build(case, server)
                secrets = forbidden_values(case, env)
                echoed = " ".join(secrets)
                server.set_default(
                    200,
                    json.dumps({"errcode": 1, "code": 1, "errmsg": echoed, "msg": echoed, "text": echoed, "errors": True, "items": [{"create": {"status": 400, "error": echoed}}]}).encode(),
                )
                with self.assertRaises(DeliveryError) as ctx:
                    next(send for name, send in send_everything(sink))()
                for secret in secrets:
                    self.assertNotIn(secret, str(ctx.exception))
                    self.assertNotIn(secret, repr(ctx.exception))


class ExporterLeakTests(unittest.TestCase):
    def test_logs_status_summary_and_state_files_are_clean(self):
        with tempfile.TemporaryDirectory() as tmp, FakeServer(echo) as server:
            cases = make_cases()
            sinks, secrets = [], set()
            for case in cases:
                sink, env = build(case, server)
                sinks.append(sink)
                secrets |= forbidden_values(case, env)
            client = StubClient()
            client.runs["orun_1"] = run_view("orun_1", "noaa-daily", "failed", [node_view("ni_1", "fetch", "2026-01-05", "failed")])
            client.lineages["orun_1"] = LINEAGE
            client.approvals = [{"approval_id": "appr_1", "kind": "egress", "state": "pending", "workflow": "noaa-daily"}]
            client.add(transition(1, "orun_1", "ni_1", "failed", cause="http.timeout"), transition(2, "orun_1", None, "failed"))
            state = Path(tmp) / "state"
            state.mkdir()
            (state / "cursor.json").write_text('{"after": 0}', encoding="utf-8")
            exporter = Exporter(client, sinks, state, sleep=noop_sleep)
            with self.assertLogs("ogun_export", level="DEBUG") as logs:
                summary = exporter.run_once()
                exporter.run_once()
            self.assertTrue(summary["sink_errors"], "every sink should have failed against the echo server")
            blobs = ["\n".join(f"{r.getMessage()} {r.args!r} {r.name}" for r in logs.records), json.dumps(summary), json.dumps(exporter.status())]
            for path in state.rglob("*"):
                if path.is_file():
                    blobs.append(path.read_text(encoding="utf-8"))
            self.assertGreater(len(blobs), 5)
            joined = "\n".join(blobs)
            for secret in secrets:
                self.assertNotIn(secret, joined)

    def test_a_sink_bug_that_raises_a_secret_is_logged_by_type_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            case = make_cases()[0]
            with FakeServer() as server:
                sink, env = build(case, server)
                secret = env["DD_API_KEY"]

                def explode(events):
                    raise RuntimeError("bad key " + secret)

                sink.send_events = explode
                client = StubClient()
                client.runs["orun_1"] = run_view("orun_1", "noaa-daily", "running", [node_view("ni_1", "fetch", "d", "failed")])
                client.add(transition(1, "orun_1", "ni_1", "failed"))
                state = Path(tmp)
                (state / "cursor.json").write_text('{"after": 0}', encoding="utf-8")
                exporter = Exporter(client, [sink], state, sleep=noop_sleep)
                with self.assertLogs("ogun_export", level="DEBUG") as logs:
                    summary = exporter.run_once()
                self.assertNotIn(secret, json.dumps(summary))
                self.assertNotIn(secret, "\n".join(r.getMessage() for r in logs.records))
                self.assertIn("RuntimeError", summary["sink_errors"]["dd"])


class CheckOutputLeakTests(unittest.TestCase):
    def test_check_prints_hosts_and_variable_names_but_never_values(self):
        sinks, env, secrets = [], {}, set()
        for case in make_cases():
            url = "https://collector.example.com"
            sinks.append(json.loads(json.dumps(case.cfg).replace("{url}", url)))
            values = case.env(url)
            env.update(values)
            secrets |= forbidden_values(case, values)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "c.json"
            path.write_text(json.dumps({"version": 1, "state_dir": str(Path(tmp) / "s"), "sinks": sinks}), encoding="utf-8")
            out, err = io.StringIO(), io.StringIO()
            code = main(["check", "--config", str(path), "--offline"], environ=env, stdout=out, stderr=err, setup_logging=False)
        self.assertEqual(code, 0, err.getvalue())
        text = out.getvalue() + err.getvalue()
        self.assertIn("collector.example.com", text)
        self.assertIn("api.datadoghq.eu", text)
        for secret in secrets:
            self.assertNotIn(secret, text)
        names = env_names(sinks)
        self.assertGreaterEqual(len(names), 15)
        for name in names:
            self.assertIn(name, text)

    def test_every_sink_type_is_covered_by_a_leak_case(self):
        self.assertEqual({c.type for c in make_cases()}, set(REGISTRY))


class LoggingConfigTests(unittest.TestCase):
    def test_the_package_logger_has_a_null_handler_so_a_library_user_sees_nothing_by_default(self):
        handlers = logging.getLogger("ogun_export").handlers
        self.assertTrue(any(isinstance(h, logging.NullHandler) for h in handlers))


if __name__ == "__main__":
    unittest.main()
