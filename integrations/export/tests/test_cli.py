import io
import json
import os
import subprocess
import sys
import tempfile
import types
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock

from fakes import FakeServer, StubClient, canary, node_view, run_view, transition
from ogun_export import cli
from ogun_export.cli import CliError, default_client_factory, main
from ogun_export.sinks import REGISTRY

SRC = str(Path(__file__).resolve().parent.parent / "src")


def run_cli(argv, env=None, factory=None):
    out, err = io.StringIO(), io.StringIO()
    code = main(argv, client_factory=factory, environ=env if env is not None else {}, stdout=out, stderr=err, setup_logging=False, sleep=lambda seconds: None)
    return code, out.getvalue(), err.getvalue()


class CliCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)

    def write_config(self, **overrides):
        cfg = {
            "version": 1,
            "credential": "export",
            "state_dir": str(self.dir / "state"),
            "sinks": [
                {"name": "dd", "type": "datadog", "api_key": {"env": "DD_API_KEY"}, "tags": ["env:prod"]},
                {"name": "chat", "type": "slack", "webhook_url": {"env": "SLACK_URL"}},
                {"name": "pd", "type": "pagerduty", "region": "eu", "routing_key": {"env": "PD_KEY"}},
            ],
        }
        cfg.update(overrides)
        path = self.dir / "export.json"
        path.write_text(json.dumps(cfg), encoding="utf-8")
        return str(path)


class SinksCommandTests(CliCase):
    def test_lists_the_whole_catalogue(self):
        code, out, err = run_cli(["sinks"])
        self.assertEqual((code, err), (0, ""))
        for name, cls in REGISTRY.items():
            self.assertIn(name, out)
            self.assertIn(cls.description, out)
        self.assertEqual(len(REGISTRY), 15)
        self.assertIn("api_key* (secret)", out)
        self.assertIn("capabilities: events, metrics", out)


class CheckCommandTests(CliCase):
    def env(self):
        return {
            "DD_API_KEY": canary(),
            "SLACK_URL": "https://hooks.slack.com/services/T0/B0/" + canary(),
            "PD_KEY": canary(),
        }

    def test_valid_configuration_lists_sinks_capabilities_and_hosts(self):
        env = self.env()
        code, out, err = run_cli(["check", "--config", self.write_config(), "--offline"], env)
        self.assertEqual((code, err), (0, ""), out)
        self.assertIn("configuration: valid, 3 sinks", out)
        self.assertIn("api.datadoghq.com", out)
        self.assertIn("hooks.slack.com", out)
        self.assertIn("events.eu.pagerduty.com", out)
        self.assertIn("events,metrics", out)
        self.assertIn("error, attention", out)
        for name in ("DD_API_KEY", "SLACK_URL", "PD_KEY"):
            self.assertRegex(out, name + r"\s+set\s")
        for value in env.values():
            self.assertNotIn(value, out + err)
        self.assertNotIn("/services/", out)

    def test_it_checks_the_daemon_with_an_agent_credential(self):
        stub = StubClient()
        seen = []
        code, out, err = run_cli(["check", "--config", self.write_config()], self.env(), lambda cfg: (seen.append(cfg.credential), stub)[1])
        self.assertEqual((code, err), (0, ""))
        self.assertIn("ogund 0.1.0-test reachable with an agent credential", out)
        self.assertEqual(seen, ["export"])
        self.assertTrue(stub.closed)
        self.assertEqual(stub.calls, ["status"])

    def test_missing_variables_are_named_without_values(self):
        env = self.env()
        del env["SLACK_URL"]
        code, out, err = run_cli(["check", "--config", self.write_config(), "--offline"], env)
        self.assertEqual(code, 2)
        self.assertRegex(out, r"SLACK_URL\s+NOT SET\s+chat")
        self.assertRegex(out, r"DD_API_KEY\s+set")
        self.assertIn("1 environment variable(s) are not set", err)
        for value in env.values():
            self.assertNotIn(value, out + err)

    def test_empty_variable_counts_as_not_set(self):
        env = self.env()
        env["PD_KEY"] = "  "
        code, out, err = run_cli(["check", "--config", self.write_config(), "--offline"], env)
        self.assertEqual(code, 2)
        self.assertRegex(out, r"PD_KEY\s+NOT SET")

    def test_literal_secret_is_refused_without_echo(self):
        literal = canary()
        cfg = self.write_config(sinks=[{"name": "dd", "type": "datadog", "api_key": literal}])
        code, out, err = run_cli(["check", "--config", cfg, "--offline"], self.env())
        self.assertEqual(code, 2)
        self.assertIn('a secret must be {"env": "NAME"}', err)
        self.assertNotIn(literal, out + err)

    def test_unknown_key_and_unreadable_file(self):
        code, out, err = run_cli(["check", "--config", self.write_config(surprise=1), "--offline"], self.env())
        self.assertEqual(code, 2)
        self.assertIn("surprise", err)
        code, out, err = run_cli(["check", "--config", str(self.dir / "nope.json"), "--offline"], self.env())
        self.assertEqual(code, 2)

    def test_an_unreachable_daemon_is_exit_1_and_reports_no_values(self):
        secret = canary()

        def factory(cfg):
            raise OSError("socket " + secret)

        code, out, err = run_cli(["check", "--config", self.write_config()], self.env(), factory)
        self.assertEqual(code, 1)
        self.assertIn("cannot reach ogund (OSError)", err)
        self.assertNotIn(secret, out + err)
        self.assertIn("configuration: valid", out)

    def test_a_refused_credential_kind_is_exit_1(self):
        def factory(cfg):
            raise CliError("the credential is not of kind agent; the exporter refuses to run with it")

        code, out, err = run_cli(["check", "--config", self.write_config()], self.env(), factory)
        self.assertEqual(code, 1)
        self.assertIn("not of kind agent", err)


class RunOnceTests(CliCase):
    def setUp(self):
        super().setUp()
        self.server = FakeServer().start()
        self.addCleanup(self.server.stop)
        self.stub = StubClient()
        self.stub.runs["orun_1"] = run_view("orun_1", "noaa-daily", "running", [node_view("ni_1", "fetch", "2026-01-05", "failed")])
        self.stub.add(transition(1, "orun_1", "ni_1", "failed", cause="http.timeout"))
        self.state = self.dir / "state"
        self.state.mkdir()
        (self.state / "cursor.json").write_text('{"after": 0}', encoding="utf-8")

    def config(self, url=None):
        return self.write_config(sinks=[{"name": "hook", "type": "webhook", "url": (url or self.server.url) + "/in"}])

    def test_one_pass_delivers_and_exits_zero(self):
        seen = []
        code, out, err = run_cli(["run", "--config", self.config(), "--once"], {}, lambda cfg: (seen.append(cfg), self.stub)[1])
        self.assertEqual((code, err), (0, ""), out)
        self.assertEqual(seen[0].credential, "export")
        self.assertIn("events: 1 seen, 1 exported", out)
        self.assertIn("sink hook: 0 pending, 0 dropped", out)
        events = [r.json() for r in self.server.requests if r.json()["type"] == "events"]
        self.assertEqual([i["seq"] for e in events for i in e["items"]], [1])
        self.assertTrue(self.stub.closed)
        self.assertLessEqual(set(self.stub.calls), {"status", "list_runs", "get_run", "events_since", "list_approvals", "lineage"})

    def test_a_sink_that_is_still_down_makes_the_exit_code_one(self):
        self.server.set_default(503)
        code, out, err = run_cli(["run", "--config", self.config(), "--once"], {}, lambda cfg: self.stub)
        self.assertEqual(code, 1)
        self.assertIn("sink hook: delivery to sink", err)
        self.assertIn("HTTP 503", err)

    def test_an_unreachable_daemon_is_exit_one(self):
        def factory(cfg):
            raise OSError("no socket")

        code, out, err = run_cli(["run", "--config", self.config(), "--once"], {}, factory)
        self.assertEqual(code, 1)
        self.assertIn("cannot reach ogund (OSError)", err)

    def test_a_refused_credential_stops_before_anything_is_sent(self):
        def factory(cfg):
            raise CliError("the credential is not of kind agent; the exporter refuses to run with it")

        code, out, err = run_cli(["run", "--config", self.config(), "--once"], {}, factory)
        self.assertEqual(code, 1)
        self.assertIn("cannot start", err)
        self.assertEqual(self.server.requests, [])

    def test_configuration_errors_are_exit_two(self):
        code, out, err = run_cli(["run", "--config", self.write_config(sinks=[]), "--once"], {}, lambda cfg: self.stub)
        self.assertEqual(code, 2)
        env_missing = self.write_config(sinks=[{"name": "dd", "type": "datadog", "api_key": {"env": "NOT_SET_ANYWHERE"}}])
        code, out, err = run_cli(["run", "--config", env_missing, "--once"], {}, lambda cfg: self.stub)
        self.assertEqual(code, 2)
        self.assertIn("NOT_SET_ANYWHERE", err)
        self.assertEqual(self.stub.calls, [])


class UsageTests(unittest.TestCase):
    def test_usage_errors_are_exit_two_and_help_is_zero(self):
        with redirect_stderr(io.StringIO()):
            self.assertEqual(run_cli([])[0], 2)
            self.assertEqual(run_cli(["frobnicate"])[0], 2)
            self.assertEqual(run_cli(["run"])[0], 2)
            self.assertEqual(run_cli(["check", "--config"])[0], 2)
            self.assertEqual(run_cli(["run", "--config", "x", "--bogus"])[0], 2)
        with mock.patch("sys.stdout", io.StringIO()):
            self.assertEqual(run_cli(["--help"])[0], 0)


class DefaultFactoryTests(unittest.TestCase):
    """The real factory, with a stand-in for the ogun_client module."""

    def fake_module(self, kind):
        calls = []
        closed = []

        class Client:
            def __init__(self):
                self.kind = kind

            @classmethod
            def connect(cls, credential=None, **kw):
                calls.append((credential, kw))
                return cls()

            def close(self):
                closed.append(True)

        module = types.ModuleType("ogun_client")
        module.Client = Client
        return module, calls, closed

    def cfg(self):
        from ogun_export.config import Config

        return Config(credential="export", ogun_home="/var/lib/ogun", profile="edge")

    def test_connects_with_the_configured_credential_and_names_itself(self):
        module, calls, closed = self.fake_module("agent")
        with mock.patch.dict(sys.modules, {"ogun_client": module}):
            client = default_client_factory(self.cfg())
        self.assertEqual(client.kind, "agent")
        (credential, kw), = calls
        self.assertEqual(credential, "export")
        self.assertEqual((kw["ogun_home"], kw["profile"], kw["client_name"]), ("/var/lib/ogun", "edge", "ogun-export"))
        self.assertNotIn("allowed_kinds", kw)
        self.assertEqual(closed, [])

    def test_a_credential_that_is_not_an_agent_is_refused_and_closed(self):
        for kind in ("human", "service", "bridge", None):
            module, calls, closed = self.fake_module(kind)
            with mock.patch.dict(sys.modules, {"ogun_client": module}):
                with self.assertRaises(CliError) as ctx:
                    default_client_factory(self.cfg())
            self.assertIn("not of kind agent", str(ctx.exception))
            self.assertEqual(closed, [True])

    def test_a_missing_ogun_client_is_a_clear_error(self):
        with mock.patch.dict(sys.modules, {"ogun_client": None}):
            with self.assertRaises(CliError) as ctx:
                default_client_factory(self.cfg())
        self.assertIn("ogun-client is not installed", str(ctx.exception))

    def test_cli_exits_one_when_the_credential_is_not_an_agent(self):
        module, calls, closed = self.fake_module("human")
        with tempfile.TemporaryDirectory() as d:
            cfg = Path(d) / "c.json"
            cfg.write_text(
                json.dumps({"version": 1, "state_dir": str(Path(d) / "s"), "sinks": [{"name": "h", "type": "webhook", "url": "http://127.0.0.1:9/in"}]}),
                encoding="utf-8",
            )
            with mock.patch.dict(sys.modules, {"ogun_client": module}):
                code, out, err = run_cli(["run", "--config", str(cfg), "--once"])
        self.assertEqual(code, 1)
        self.assertIn("not of kind agent", err)


class ImportTests(unittest.TestCase):
    def test_the_library_does_not_import_ogun_client(self):
        code = (
            "import sys; sys.path.insert(0, %r);" % SRC
            + "import ogun_export.runner, ogun_export.config, ogun_export.sinks, ogun_export.outbox, ogun_export.cli;"
            + "assert 'ogun_client' not in sys.modules, 'ogun_client was imported';"
            + "import os; ogun_export.cli.main(['sinks'], stdout=open(os.devnull, 'w'));"
            + "assert 'ogun_client' not in sys.modules, 'ogun_client was imported by the sinks command'"
        )
        done = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, cwd=tempfile.gettempdir())
        self.assertEqual(done.returncode, 0, done.stderr)


if __name__ == "__main__":
    unittest.main()
