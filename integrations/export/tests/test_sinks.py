"""Every sink against a loopback fake: path, method, headers and body shape, written from public docs."""

import base64
import calendar
import hashlib
import hmac
import json
import unittest
import urllib.parse
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Tuple

from fakes import LINEAGE, FakeServer, attention_event, canary, failed_event, noop_sleep, sample_snapshot
from ogun_export.http import DeliveryError
from ogun_export.model import ExportEvent
from ogun_export.secrets import ConfigError
from ogun_export.sinks import REGISTRY
from ogun_export.sinks.base import Sink

TS_EVENT = calendar.timegm((2026, 10, 24, 12, 0, 0))
TS_SNAPSHOT = calendar.timegm((2026, 10, 24, 12, 1, 0))
CLOCK = 1_700_000_000.0


@dataclass
class Ctx:
    url: str
    env: Dict[str, str]
    sink: Sink


@dataclass
class Case:
    type: str
    cfg: Dict[str, Any]
    env: Callable[[str], Dict[str, str]]
    point: Callable[[Sink, str], None]
    reply: Tuple[int, bytes, Dict[str, str]]
    events: Optional[Tuple[int, Callable]] = None
    snapshot: Optional[Tuple[int, Callable]] = None
    lineage: Optional[Tuple[int, Callable]] = None
    secrets: List[str] = field(default_factory=list)


def no_point(sink: Sink, url: str) -> None:
    pass


def json_docs(raw: bytes) -> List[Any]:
    """Concatenated JSON objects, as Splunk HEC takes them."""
    text, pos, out = raw.decode("utf-8"), 0, []
    decoder = json.JSONDecoder()
    while pos < len(text):
        doc, pos = decoder.raw_decode(text, pos)
        out.append(doc)
    return out


# ---- per-sink checks ----


def check_datadog_events(t, reqs, c):
    for r in reqs:
        t.assertEqual((r.method, r.route), ("POST", "/api/v1/events"))
        t.assertEqual(r.header("dd-api-key"), c.env["DD_API_KEY"])
        t.assertEqual(r.header("content-type"), "application/json")
    body = reqs[0].json()
    t.assertEqual(
        body,
        {
            "title": failed_event().title,
            "text": failed_event().text,
            "alert_type": "error",
            "tags": ["service:ogun", "env:prod", "workflow:noaa-daily", "node:fetch"],
            "aggregation_key": "orun_1:fetch:2026-01-05",
            "source_type_name": "ogun",
            "date_happened": TS_EVENT,
        },
    )
    t.assertEqual(reqs[1].json()["alert_type"], "warning")
    t.assertEqual(reqs[1].json()["tags"][-1], "node:review")


def check_datadog_snapshot(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("POST", "/api/v2/series"))
    t.assertEqual(r.header("dd-api-key"), c.env["DD_API_KEY"])
    series = r.json()["series"]
    t.assertEqual(
        [(s["metric"], s["points"][0]["value"]) for s in series],
        [("ogun.workflows", 3), ("ogun.runs.active", 2), ("ogun.approvals.pending", 1), ("ogun.streams", 4), ("ogun.node_instances", 2), ("ogun.node_instances", 1)],
    )
    for s in series:
        t.assertEqual(s["type"], 3)
        t.assertEqual(s["points"][0]["timestamp"], TS_SNAPSHOT)
        t.assertEqual(s["resources"], [{"name": "host-1", "type": "host"}])
        t.assertEqual(s["tags"][:2], ["service:ogun", "env:prod"])
    t.assertEqual(series[4]["tags"][-1], "state:running")
    t.assertEqual(series[5]["tags"][-1], "state:failed")


def check_openlineage(t, reqs, c):
    for r, expected in zip(reqs, LINEAGE):
        t.assertEqual((r.method, r.route), ("POST", "/api/v1/lineage"))
        t.assertEqual(r.header("content-type"), "application/json")
        t.assertEqual(r.header("authorization"), "Bearer " + c.env["OL_TOKEN"])
        t.assertEqual(r.header("x-api-token"), c.env["OL_HEADER"])
        t.assertEqual(r.json(), expected)


def check_slack(t, reqs, c):
    for r in reqs:
        t.assertEqual((r.method, r.route), ("POST", urllib.parse.urlsplit(c.env["SLACK_URL"]).path))
        t.assertEqual(r.header("content-type"), "application/json")
    body = reqs[0].json()
    t.assertEqual(body["text"], f"{failed_event().title}: {failed_event().text}".replace(">", "&gt;"))
    t.assertEqual(body["blocks"][0]["type"], "section")
    t.assertIn("*" + failed_event().title + "*", body["blocks"][0]["text"]["text"])
    t.assertEqual(body["blocks"][1]["type"], "context")
    t.assertIn("severity: error", body["blocks"][1]["elements"][0]["text"])


def check_teams(t, reqs, c):
    for r in reqs:
        t.assertEqual((r.method, r.route), ("POST", urllib.parse.urlsplit(c.env["TEAMS_URL"]).path))
    body = reqs[0].json()
    t.assertEqual(body["type"], "message")
    (att,) = body["attachments"]
    t.assertEqual(att["contentType"], "application/vnd.microsoft.card.adaptive")
    card = att["content"]
    t.assertEqual((card["type"], card["version"]), ("AdaptiveCard", "1.4"))
    t.assertEqual(card["body"][0]["text"], failed_event().title)
    t.assertEqual(card["body"][-1]["type"], "FactSet")
    t.assertIn({"title": "Severity", "value": "error"}, card["body"][-1]["facts"])


def check_pagerduty(t, reqs, c):
    for r in reqs:
        t.assertEqual((r.method, r.route), ("POST", "/v2/enqueue"))
        t.assertEqual(r.header("content-type"), "application/json")
    first = reqs[0].json()
    t.assertEqual(first["routing_key"], c.env["PD_KEY"])
    t.assertEqual(first["event_action"], "trigger")
    t.assertEqual(first["dedup_key"], "orun_1:fetch:2026-01-05")
    t.assertEqual(first["payload"]["summary"], failed_event().title)
    t.assertEqual((first["payload"]["source"], first["payload"]["severity"]), ("ogun", "error"))
    t.assertEqual(first["payload"]["custom_details"]["workflow"], "noaa-daily")
    second = reqs[1].json()
    t.assertEqual((second["event_action"], second["payload"]["severity"]), ("trigger", "warning"))


def check_dingtalk(t, reqs, c):
    for r in reqs:
        t.assertEqual((r.method, r.route), ("POST", "/robot/send"))
        params = urllib.parse.parse_qs(r.query)
        t.assertEqual(params["access_token"], [c.env["DING_TOKEN_VALUE"]])
        t.assertEqual(params["timestamp"], ["1700000000000"])
        key = c.env["DING_SECRET"]
        digest = hmac.new(key.encode(), f"1700000000000\n{key}".encode(), hashlib.sha256).digest()
        t.assertEqual(params["sign"], [base64.b64encode(digest).decode()])
        t.assertIn("sign=" + urllib.parse.quote_plus(base64.b64encode(digest)), r.query)
    body = reqs[0].json()
    t.assertEqual(body["msgtype"], "markdown")
    t.assertTrue(body["markdown"]["title"].startswith("Ogun: "))
    t.assertIn(failed_event().title, body["markdown"]["text"])


def check_feishu(t, reqs, c):
    for r in reqs:
        t.assertEqual((r.method, r.route), ("POST", urllib.parse.urlsplit(c.env["FEISHU_URL"]).path))
        body = r.json()
        key = f"1700000000\n{c.env['FEISHU_SECRET']}".encode()
        t.assertEqual(body["timestamp"], "1700000000")
        t.assertEqual(body["sign"], base64.b64encode(hmac.new(key, b"", hashlib.sha256).digest()).decode())
        t.assertEqual(body["msg_type"], "text")
    t.assertIn(failed_event().title, reqs[0].json()["content"]["text"])


def check_wecom(t, reqs, c):
    for r in reqs:
        t.assertEqual((r.method, r.route), ("POST", "/cgi-bin/webhook/send"))
        t.assertEqual(urllib.parse.parse_qs(r.query)["key"], [c.env["WECOM_KEY_VALUE"]])
    body = reqs[0].json()
    t.assertEqual(body["msgtype"], "markdown")
    t.assertIn(failed_event().title, body["markdown"]["content"])


def webhook_signature(secret: str, body: bytes) -> str:
    return "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()


def check_webhook_common(t, r, c):
    t.assertEqual((r.method, r.route), ("POST", "/ingest"))
    t.assertEqual(r.header("content-type"), "application/json")
    t.assertEqual(r.header("x-ogun-signature"), webhook_signature(c.env["WH_SECRET"], r.body))
    t.assertEqual(r.header("authorization"), "Bearer " + c.env["WH_AUTH"])
    t.assertEqual(r.json()["sent_at"], "2023-11-14T22:13:20.000Z")


def check_webhook_events(t, reqs, c):
    (r,) = reqs
    check_webhook_common(t, r, c)
    body = r.json()
    t.assertEqual(body["type"], "events")
    t.assertEqual(body["items"], [failed_event().to_dict(), attention_event().to_dict()])


def check_webhook_snapshot(t, reqs, c):
    (r,) = reqs
    check_webhook_common(t, r, c)
    t.assertEqual(r.json()["type"], "snapshot")
    t.assertEqual(r.json()["snapshot"], sample_snapshot().to_dict())


def check_webhook_lineage(t, reqs, c):
    (r,) = reqs
    check_webhook_common(t, r, c)
    t.assertEqual((r.json()["type"], r.json()["items"]), ("lineage", LINEAGE))


def check_splunk_events(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("POST", "/services/collector/event"))
    t.assertEqual(r.header("authorization"), "Splunk " + c.env["SPLUNK_TOKEN"])
    docs = json_docs(r.body)
    t.assertEqual(len(docs), 2)
    first = docs[0]
    t.assertEqual((first["source"], first["sourcetype"], first["host"], first["index"]), ("ogun", "ogun:event", "splunk-host", "ogun"))
    t.assertEqual(first["time"], TS_EVENT + 0.123)
    t.assertEqual((first["event"]["kind"], first["event"]["severity"], first["event"]["run_id"]), ("node_transition", "error", "orun_1"))


def check_splunk_snapshot(t, reqs, c):
    (r,) = reqs
    t.assertEqual(r.route, "/services/collector/event")
    (doc,) = json_docs(r.body)
    t.assertEqual(doc["sourcetype"], "ogun:snapshot")
    t.assertEqual(doc["time"], float(TS_SNAPSHOT))
    t.assertEqual(doc["event"]["runs_active"], 2)
    t.assertEqual(doc["event"]["node_instances_by_state"], {"running": 2, "failed": 1})


def check_prometheus(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("PUT", "/metrics/job/ogun/instance/host1"))
    t.assertEqual(r.header("content-type"), "text/plain; version=0.0.4; charset=utf-8")
    pair = f"{c.env['PROM_USER']}:{c.env['PROM_PASS']}".encode()
    t.assertEqual(r.header("authorization"), "Basic " + base64.b64encode(pair).decode())
    lines = r.body.decode().split("\n")
    t.assertEqual(lines[-1], "")
    for line in (
        "# TYPE ogun_workflows gauge",
        "# TYPE ogun_runs_active gauge",
        "# TYPE ogun_approvals_pending gauge",
        "# TYPE ogun_streams gauge",
        "# TYPE ogun_node_instances gauge",
        "ogun_workflows 3",
        "ogun_runs_active 2",
        "ogun_approvals_pending 1",
        "ogun_streams 4",
        'ogun_node_instances{state="running"} 2',
        'ogun_node_instances{state="failed"} 1',
    ):
        t.assertIn(line, lines)
    t.assertEqual(lines.count("# TYPE ogun_node_instances gauge"), 1)


def bulk_lines(r) -> List[Any]:
    text = r.body.decode()
    assert text.endswith("\n")
    return [json.loads(line) for line in text.splitlines()]


def check_elastic_events(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("POST", "/_bulk"))
    t.assertEqual(r.header("content-type"), "application/x-ndjson")
    t.assertEqual(r.header("authorization"), "ApiKey " + c.env["ES_KEY"])
    lines = bulk_lines(r)
    t.assertEqual(len(lines), 4)
    t.assertEqual(lines[0]["create"]["_index"], "ogun-events")
    t.assertTrue(lines[0]["create"]["_id"])
    t.assertNotEqual(lines[0]["create"]["_id"], lines[2]["create"]["_id"])
    doc = lines[1]
    t.assertEqual(doc["@timestamp"], failed_event().at)
    t.assertEqual(doc["service"], {"name": "ogun"})
    t.assertEqual(doc["log"]["level"], "error")
    t.assertEqual((doc["ogun"]["run_id"], doc["ogun"]["workflow"], doc["ogun"]["node"]), ("orun_1", "noaa-daily", "fetch"))
    t.assertEqual(lines[3]["log"]["level"], "warning")


def check_elastic_snapshot(t, reqs, c):
    (r,) = reqs
    lines = bulk_lines(r)
    t.assertEqual(lines[0]["create"]["_index"], "ogun-metrics")
    t.assertEqual(lines[1]["@timestamp"], sample_snapshot().taken_at)
    t.assertEqual(lines[1]["ogun"]["runs_active"], 2)


def attr_map(attrs):
    return {a["key"]: a["value"]["stringValue"] for a in attrs}


def check_otlp_events(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("POST", "/v1/logs"))
    t.assertEqual(r.header("content-type"), "application/json")
    t.assertEqual(r.header("x-otlp-token"), c.env["OTLP_TOKEN"])
    (rl,) = r.json()["resourceLogs"]
    t.assertEqual(attr_map(rl["resource"]["attributes"]), {"service.name": "ogun"})
    (sl,) = rl["scopeLogs"]
    records = sl["logRecords"]
    t.assertEqual(len(records), 2)
    t.assertEqual((records[0]["severityNumber"], records[0]["severityText"]), (17, "ERROR"))
    t.assertEqual((records[1]["severityNumber"], records[1]["severityText"]), (13, "WARN"))
    t.assertEqual(records[0]["timeUnixNano"], str(TS_EVENT * 10**9 + 123_000_000))
    t.assertIn(failed_event().title, records[0]["body"]["stringValue"])
    attrs = attr_map(records[0]["attributes"])
    t.assertEqual((attrs["ogun.workflow"], attrs["ogun.node"], attrs["ogun.kind"]), ("noaa-daily", "fetch", "node_transition"))


def check_otlp_snapshot(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("POST", "/v1/metrics"))
    (rm,) = r.json()["resourceMetrics"]
    t.assertEqual(attr_map(rm["resource"]["attributes"]), {"service.name": "ogun"})
    metrics = {m["name"]: m for m in rm["scopeMetrics"][0]["metrics"]}
    t.assertEqual(set(metrics), {"ogun.workflows", "ogun.runs.active", "ogun.approvals.pending", "ogun.streams", "ogun.node_instances"})
    point = metrics["ogun.runs.active"]["gauge"]["dataPoints"][0]
    t.assertEqual((point["asInt"], point["timeUnixNano"]), ("2", str(TS_SNAPSHOT * 10**9)))
    states = metrics["ogun.node_instances"]["gauge"]["dataPoints"]
    t.assertEqual([(attr_map(p["attributes"])["state"], p["asInt"]) for p in states], [("running", "2"), ("failed", "1")])


def check_newrelic_events(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("POST", "/log/v1"))
    t.assertEqual(r.header("api-key"), c.env["NR_KEY"])
    t.assertEqual(r.header("content-type"), "application/json")
    (batch,) = r.json()
    t.assertEqual(batch["common"], {"attributes": {"service.name": "ogun"}})
    first, second = batch["logs"]
    t.assertEqual(first["timestamp"], TS_EVENT * 1000 + 123)
    t.assertIn(failed_event().title, first["message"])
    t.assertEqual((first["attributes"]["level"], second["attributes"]["level"]), ("error", "warn"))
    t.assertEqual((first["attributes"]["workflow"], first["attributes"]["node"]), ("noaa-daily", "fetch"))


def check_newrelic_snapshot(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("POST", "/metric/v1"))
    t.assertEqual(r.header("api-key"), c.env["NR_KEY"])
    (batch,) = r.json()
    t.assertEqual(batch["common"], {"attributes": {"service.name": "ogun"}})
    metrics = batch["metrics"]
    t.assertEqual(metrics[1], {"name": "ogun.runs.active", "type": "gauge", "value": 2, "timestamp": TS_SNAPSHOT * 1000})
    t.assertEqual(metrics[4]["attributes"], {"state": "running"})
    t.assertEqual(len(metrics), 6)


def check_loki(t, reqs, c):
    (r,) = reqs
    t.assertEqual((r.method, r.route), ("POST", "/loki/api/v1/push"))
    t.assertEqual(r.header("content-type"), "application/json")
    t.assertEqual(r.header("authorization"), "Bearer " + c.env["LOKI_TOKEN"])
    t.assertEqual(r.header("x-scope-orgid"), "tenant1")
    streams = r.json()["streams"]
    by_level = {s["stream"]["level"]: s for s in streams}
    t.assertEqual(set(by_level), {"error", "warn"})
    err = by_level["error"]
    t.assertEqual(err["stream"], {"service_name": "ogun", "level": "error", "workflow": "noaa-daily"})
    ((ns, line),) = err["values"]
    t.assertEqual(ns, str(TS_EVENT * 10**9 + 123_000_000))
    t.assertEqual(json.loads(line)["run_id"], "orun_1")


def make_cases() -> List[Case]:
    k = {name: canary() for name in "DD OL OLH SLACK TEAMS PD DING DINGSEC FEISHU FEISHUSEC WECOM WH WHAUTH SPLUNK PROMU PROMP ES OTLP NR LOKI".split()}
    values = lambda *names: [k[n] for n in names]  # noqa: E731
    return [
        Case(
            "datadog",
            {"name": "dd", "type": "datadog", "site": "datadoghq.eu", "api_key": {"env": "DD_API_KEY"}, "tags": ["env:prod"], "host": "host-1"},
            lambda url: {"DD_API_KEY": k["DD"]},
            lambda s, url: setattr(s, "base_url", url),
            (202, b'{"status":"ok"}', {}),
            events=(2, check_datadog_events),
            snapshot=(1, check_datadog_snapshot),
            secrets=values("DD"),
        ),
        Case(
            "openlineage",
            {
                "name": "ol",
                "type": "openlineage",
                "url": "{url}",
                "auth": {"env": "OL_TOKEN"},
                "header_name": "X-Api-Token",
                "header_value_env": {"env": "OL_HEADER"},
            },
            lambda url: {"OL_TOKEN": k["OL"], "OL_HEADER": k["OLH"]},
            no_point,
            (201, b"", {}),
            lineage=(2, check_openlineage),
            secrets=values("OL", "OLH"),
        ),
        Case(
            "slack",
            {"name": "chat", "type": "slack", "webhook_url": {"env": "SLACK_URL"}},
            lambda url: {"SLACK_URL": f"{url}/services/T0/B0/{k['SLACK']}"},
            no_point,
            (200, b"ok", {}),
            events=(2, check_slack),
            secrets=values("SLACK"),
        ),
        Case(
            "teams",
            {"name": "teams", "type": "teams", "webhook_url": {"env": "TEAMS_URL"}},
            lambda url: {"TEAMS_URL": f"{url}/workflows/{k['TEAMS']}/triggers/manual/paths/invoke"},
            no_point,
            (202, b"", {}),
            events=(2, check_teams),
            secrets=values("TEAMS"),
        ),
        Case(
            "pagerduty",
            {"name": "pd", "type": "pagerduty", "region": "eu", "routing_key": {"env": "PD_KEY"}},
            lambda url: {"PD_KEY": k["PD"]},
            lambda s, url: setattr(s, "enqueue_url", url + "/v2/enqueue"),
            (202, b'{"status":"success","message":"Event processed","dedup_key":"x"}', {}),
            events=(2, check_pagerduty),
            secrets=values("PD"),
        ),
        Case(
            "dingtalk",
            {"name": "ding", "type": "dingtalk", "webhook_url": {"env": "DING_URL"}, "secret": {"env": "DING_SECRET"}},
            lambda url: {"DING_URL": f"{url}/robot/send?access_token={k['DING']}", "DING_SECRET": k["DINGSEC"], "DING_TOKEN_VALUE": k["DING"]},
            no_point,
            (200, b'{"errcode":0,"errmsg":"ok"}', {}),
            events=(2, check_dingtalk),
            secrets=values("DING", "DINGSEC"),
        ),
        Case(
            "feishu",
            {"name": "feishu", "type": "feishu", "webhook_url": {"env": "FEISHU_URL"}, "secret": {"env": "FEISHU_SECRET"}},
            lambda url: {"FEISHU_URL": f"{url}/open-apis/bot/v2/hook/{k['FEISHU']}", "FEISHU_SECRET": k["FEISHUSEC"]},
            no_point,
            (200, b'{"code":0,"msg":"success","data":{}}', {}),
            events=(2, check_feishu),
            secrets=values("FEISHU", "FEISHUSEC"),
        ),
        Case(
            "wecom",
            {"name": "wecom", "type": "wecom", "webhook_url": {"env": "WECOM_URL"}},
            lambda url: {"WECOM_URL": f"{url}/cgi-bin/webhook/send?key={k['WECOM']}", "WECOM_KEY_VALUE": k["WECOM"]},
            no_point,
            (200, b'{"errcode":0,"errmsg":"ok"}', {}),
            events=(2, check_wecom),
            secrets=values("WECOM"),
        ),
        Case(
            "webhook",
            {
                "name": "hook",
                "type": "webhook",
                "url": "{url}/ingest",
                "signing_secret": {"env": "WH_SECRET"},
                "headers_env": {"Authorization": {"env": "WH_AUTH_HEADER"}},
            },
            lambda url: {"WH_SECRET": k["WH"], "WH_AUTH_HEADER": "Bearer " + k["WHAUTH"], "WH_AUTH": k["WHAUTH"]},
            no_point,
            (200, b"", {}),
            events=(1, check_webhook_events),
            snapshot=(1, check_webhook_snapshot),
            lineage=(1, check_webhook_lineage),
            secrets=values("WH", "WHAUTH"),
        ),
        Case(
            "splunk",
            {"name": "splunk", "type": "splunk", "url": "{url}", "token": {"env": "SPLUNK_TOKEN"}, "index": "ogun", "host": "splunk-host"},
            lambda url: {"SPLUNK_TOKEN": k["SPLUNK"]},
            no_point,
            (200, b'{"text":"Success","code":0}', {}),
            events=(1, check_splunk_events),
            snapshot=(1, check_splunk_snapshot),
            secrets=values("SPLUNK"),
        ),
        Case(
            "prometheus",
            {
                "name": "prom",
                "type": "prometheus",
                "url": "{url}",
                "instance": "host1",
                "username": {"env": "PROM_USER"},
                "password": {"env": "PROM_PASS"},
            },
            lambda url: {"PROM_USER": k["PROMU"], "PROM_PASS": k["PROMP"]},
            no_point,
            (200, b"", {}),
            snapshot=(1, check_prometheus),
            secrets=values("PROMU", "PROMP"),
        ),
        Case(
            "elastic",
            {"name": "es", "type": "elastic", "url": "{url}", "api_key": {"env": "ES_KEY"}},
            lambda url: {"ES_KEY": k["ES"]},
            no_point,
            (200, b'{"took":1,"errors":false,"items":[]}', {}),
            events=(1, check_elastic_events),
            snapshot=(1, check_elastic_snapshot),
            secrets=values("ES"),
        ),
        Case(
            "otlp",
            {"name": "otel", "type": "otlp", "endpoint": "{url}", "headers_env": {"X-Otlp-Token": {"env": "OTLP_TOKEN"}}},
            lambda url: {"OTLP_TOKEN": k["OTLP"]},
            no_point,
            (200, b"{}", {}),
            events=(1, check_otlp_events),
            snapshot=(1, check_otlp_snapshot),
            secrets=values("OTLP"),
        ),
        Case(
            "newrelic",
            {"name": "nr", "type": "newrelic", "region": "eu", "api_key": {"env": "NR_KEY"}},
            lambda url: {"NR_KEY": k["NR"]},
            lambda s, url: (setattr(s, "log_url", url + "/log/v1"), setattr(s, "metric_url", url + "/metric/v1")),
            (202, b'{"requestId":"abc"}', {}),
            events=(1, check_newrelic_events),
            snapshot=(1, check_newrelic_snapshot),
            secrets=values("NR"),
        ),
        Case(
            "loki",
            {"name": "loki", "type": "loki", "url": "{url}", "token": {"env": "LOKI_TOKEN"}, "tenant_id": "tenant1"},
            lambda url: {"LOKI_TOKEN": k["LOKI"]},
            no_point,
            (204, b"", {}),
            events=(1, check_loki),
            secrets=values("LOKI"),
        ),
    ]


def build(case: Case, server: FakeServer) -> Tuple[Sink, Dict[str, str]]:
    """The sink for a case, pointed at the fake, with a fixed clock and no sleeping."""
    cfg = json.loads(json.dumps(case.cfg).replace("{url}", server.url))
    env = case.env(server.url)
    sink = REGISTRY[case.type].from_config(cfg, env)
    case.point(sink, server.url)
    sink.sleep = noop_sleep
    sink.clock = lambda: CLOCK
    return sink, env


class SinkTableTests(unittest.TestCase):
    def test_every_sink_type_has_a_case(self):
        self.assertEqual({c.type for c in make_cases()}, set(REGISTRY))

    def test_requests_match_each_service_s_documented_shape(self):
        for case in make_cases():
            with self.subTest(sink=case.type), FakeServer() as server:
                server.set_default(*case.reply)
                sink, env = build(case, server)
                ctx = Ctx(server.url, env, sink)
                steps = (
                    ("events", case.events, lambda: sink.send_events([failed_event(), attention_event()])),
                    ("metrics", case.snapshot, lambda: sink.send_snapshot(sample_snapshot())),
                    ("lineage", case.lineage, lambda: sink.send_lineage(LINEAGE)),
                )
                declared = {name for name, spec, _ in steps if spec is not None}
                self.assertEqual(declared, set(sink.capabilities))
                for name, spec, send in steps:
                    server.requests.clear()
                    if spec is None:
                        with self.assertRaises(NotImplementedError):
                            send()
                        self.assertEqual(server.requests, [])
                        continue
                    expected, check = spec
                    send()
                    self.assertEqual(len(server.requests), expected, f"{case.type} {name}")
                    check(self, list(server.requests), ctx)

    def test_every_declared_capability_is_implemented_by_the_class(self):
        for name, cls in REGISTRY.items():
            with self.subTest(sink=name):
                for cap, method in (("events", "send_events"), ("metrics", "send_snapshot"), ("lineage", "send_lineage")):
                    overridden = getattr(cls, method) is not getattr(Sink, method)
                    self.assertEqual(overridden, cap in cls.capabilities, f"{name}.{method}")

    def test_every_sink_reports_where_it_will_connect(self):
        for case in make_cases():
            with self.subTest(sink=case.type):
                cls = REGISTRY[case.type]
                env = case.env("https://collector.example.com")
                cfg = json.loads(json.dumps(case.cfg).replace("{url}", "https://collector.example.com"))
                hosts = cls.egress_hosts(cfg, env)
                self.assertTrue(hosts)
                for h in hosts:
                    self.assertNotIn("/", h)
                    self.assertNotIn("OGUN-CANARY", h)

    def test_known_egress_hosts(self):
        env = {"K": "x", "U": "https://hooks.slack.com/services/T/B/x"}
        self.assertEqual(REGISTRY["datadog"].egress_hosts({"site": "ddog-gov.com"}), ["api.ddog-gov.com"])
        self.assertEqual(REGISTRY["datadog"].egress_hosts({}), ["api.datadoghq.com"])
        self.assertEqual(REGISTRY["pagerduty"].egress_hosts({"region": "eu"}), ["events.eu.pagerduty.com"])
        self.assertEqual(REGISTRY["pagerduty"].egress_hosts({}), ["events.pagerduty.com"])
        self.assertEqual(REGISTRY["newrelic"].egress_hosts({"region": "eu"}), ["metric-api.eu.newrelic.com", "log-api.eu.newrelic.com"])
        self.assertEqual(REGISTRY["newrelic"].egress_hosts({}), ["metric-api.newrelic.com", "log-api.newrelic.com"])
        self.assertEqual(REGISTRY["slack"].egress_hosts({"webhook_url": {"env": "U"}}, env), ["hooks.slack.com"])
        self.assertEqual(REGISTRY["slack"].egress_hosts({"webhook_url": {"env": "MISSING"}}, env), ["hooks.slack.com"])
        self.assertEqual(REGISTRY["splunk"].egress_hosts({"url": "https://splunk.example.com:8088"}), ["splunk.example.com:8088"])
        self.assertEqual(REGISTRY["dingtalk"].egress_hosts({"webhook_url": {"env": "MISSING"}}, {}), ["oapi.dingtalk.com"])


class SeverityFilterTests(unittest.TestCase):
    def sink(self, **extra):
        cfg = {"name": "hook", "type": "webhook", "url": "http://127.0.0.1:9/x"}
        cfg.update(extra)
        return REGISTRY["webhook"].from_config(cfg, {})

    def event(self, severity, kind="node_transition"):
        return ExportEvent(kind=kind, at="2026-10-24T12:00:00.000Z", severity=severity)

    def test_default_is_warning_plus_attention_and_run_finished(self):
        s = self.sink()
        self.assertEqual(s.min_severity, "warning")
        self.assertEqual(
            [s.accepts(self.event(v)) for v in ("info", "warning", "error", "attention")],
            [False, True, True, True],
        )
        self.assertTrue(s.accepts(self.event("info", kind="run_finished")))

    def test_min_severity_error_still_passes_attention(self):
        s = self.sink(min_severity="error")
        self.assertEqual([s.accepts(self.event(v)) for v in ("info", "warning", "error", "attention")], [False, False, True, True])

    def test_min_severity_info_passes_everything(self):
        s = self.sink(min_severity="info")
        self.assertTrue(all(s.accepts(self.event(v)) for v in ("info", "warning", "error", "attention")))

    def test_sinks_without_events_accept_nothing(self):
        prom = REGISTRY["prometheus"].from_config({"name": "p", "type": "prometheus", "url": "http://127.0.0.1:9"}, {})
        self.assertFalse(prom.accepts(self.event("error")))
        self.assertFalse(prom.accepts(self.event("info", kind="run_finished")))

    def test_pagerduty_takes_error_and_attention_and_recoveries_only(self):
        pd = REGISTRY["pagerduty"].from_config({"name": "pd", "type": "pagerduty", "routing_key": {"env": "K"}}, {"K": "k"})
        self.assertEqual([pd.accepts(self.event(v)) for v in ("info", "warning", "error", "attention")], [False, False, True, True])
        self.assertTrue(pd.accepts(self.event("info", kind="node_recovered")))
        self.assertTrue(pd.accepts(self.event("info", kind="approval_resolved")))
        self.assertFalse(pd.accepts(self.event("info", kind="run_finished")))


class ProtocolDetailTests(unittest.TestCase):
    """Behaviour of single sinks that the table does not cover."""

    def setUp(self):
        self.server = FakeServer().start()
        self.addCleanup(self.server.stop)

    def make(self, type_, cfg, env=None):
        full = {"name": "s1", "type": type_}
        full.update(json.loads(json.dumps(cfg).replace("{url}", self.server.url)))
        sink = REGISTRY[type_].from_config(full, env or {})
        sink.sleep = noop_sleep
        sink.clock = lambda: CLOCK
        return sink

    # ---- chat sinks that answer 200 with an error code ----

    def test_dingtalk_errcode_is_a_failure_and_reports_only_the_code(self):
        token = canary()
        self.server.set_default(200, json.dumps({"errcode": 310000, "errmsg": f"keywords not in content, url ...access_token={token}"}).encode())
        sink = self.make("dingtalk", {"webhook_url": {"env": "U"}}, {"U": f"{self.server.url}/robot/send?access_token={token}"})
        with self.assertRaises(DeliveryError) as ctx:
            sink.send_events([failed_event()])
        message = str(ctx.exception)
        self.assertIn("errcode 310000", message)
        self.assertIn('"s1"', message)
        self.assertNotIn(token, message)
        self.assertNotIn("keywords", message)
        self.assertEqual(len(self.server.requests), 1)

    def test_dingtalk_without_a_secret_sends_no_signature(self):
        self.server.set_default(200, b'{"errcode":0}')
        sink = self.make("dingtalk", {"webhook_url": {"env": "U"}}, {"U": f"{self.server.url}/robot/send?access_token=abc"})
        sink.send_events([failed_event()])
        self.assertEqual(self.server.requests[0].path, "/robot/send?access_token=abc")

    def test_dingtalk_unparseable_reply_is_a_failure(self):
        self.server.set_default(200, b"<html>")
        sink = self.make("dingtalk", {"webhook_url": {"env": "U"}}, {"U": f"{self.server.url}/robot/send?access_token=abc"})
        with self.assertRaises(DeliveryError):
            sink.send_events([failed_event()])

    def test_wecom_errcode_is_a_failure(self):
        self.server.set_default(200, b'{"errcode":93000,"errmsg":"invalid webhook url"}')
        sink = self.make("wecom", {"webhook_url": {"env": "U"}}, {"U": f"{self.server.url}/cgi-bin/webhook/send?key=abc"})
        with self.assertRaises(DeliveryError) as ctx:
            sink.send_events([failed_event()])
        self.assertIn("errcode 93000", str(ctx.exception))
        self.assertNotIn("invalid webhook url", str(ctx.exception))

    def test_wecom_content_is_cut_to_the_documented_size(self):
        self.server.set_default(200, b'{"errcode":0}')
        sink = self.make("wecom", {"webhook_url": {"env": "U"}}, {"U": f"{self.server.url}/cgi-bin/webhook/send?key=abc"})
        sink.send_events([failed_event(text="é" * 5000)])
        content = self.server.requests[0].json()["markdown"]["content"]
        self.assertLessEqual(len(content.encode("utf-8")), 4096)

    def test_feishu_code_is_a_failure(self):
        sink = self.make("feishu", {"webhook_url": {"env": "U"}}, {"U": f"{self.server.url}/open-apis/bot/v2/hook/abc"})
        for body, ok in (
            (b'{"code":0,"msg":"success"}', True),
            (b'{"StatusCode":0,"StatusMessage":"success"}', True),
            (b'{"code":19021,"msg":"sign match fail or timestamp is not within one hour from current time"}', False),
            (b'{"StatusCode":9499,"StatusMessage":"Bad Request"}', False),
            (b"{}", False),
            (b"not json", False),
            (b'{"code":false}', False),
        ):
            with self.subTest(body=body):
                self.server.set_default(200, body)
                if ok:
                    sink.send_events([failed_event()])
                else:
                    with self.assertRaises(DeliveryError) as ctx:
                        sink.send_events([failed_event()])
                    self.assertNotIn("sign match", str(ctx.exception))

    def test_feishu_without_a_secret_sends_no_signature(self):
        self.server.set_default(200, b'{"code":0}')
        sink = self.make("feishu", {"webhook_url": {"env": "U"}}, {"U": f"{self.server.url}/open-apis/bot/v2/hook/abc"})
        sink.send_events([failed_event()])
        self.assertEqual(set(self.server.requests[0].json()), {"msg_type", "content"})

    def test_splunk_code_is_a_failure(self):
        sink = self.make("splunk", {"url": "{url}", "token": {"env": "T"}}, {"T": "tok"})
        for body, ok in ((b'{"text":"Success","code":0}', True), (b'{"text":"Invalid token","code":4}', False), (b"", False)):
            with self.subTest(body=body):
                self.server.set_default(200, body)
                if ok:
                    sink.send_events([failed_event()])
                else:
                    with self.assertRaises(DeliveryError) as ctx:
                        sink.send_events([failed_event()])
                    self.assertNotIn("Invalid token", str(ctx.exception))

    def test_elastic_errors_are_reported_with_counts_only(self):
        reply = {
            "took": 3,
            "errors": True,
            "items": [
                {"create": {"_index": "ogun-events", "_id": "a", "status": 201}},
                {"create": {"_index": "ogun-events", "_id": "b", "status": 400, "error": {"type": "mapper_parsing_exception", "reason": "SECRET-ish reason"}}},
            ],
        }
        self.server.set_default(200, json.dumps(reply).encode())
        sink = self.make("elastic", {"url": "{url}", "api_key": {"env": "K"}}, {"K": "k"})
        with self.assertRaises(DeliveryError) as ctx:
            sink.send_events([failed_event(), attention_event()])
        self.assertIn("1 of 2 items", str(ctx.exception))
        self.assertNotIn("mapper_parsing_exception", str(ctx.exception))
        self.assertNotIn("SECRET-ish", str(ctx.exception))

    def test_elastic_errors_true_without_items_is_a_failure(self):
        self.server.set_default(200, b'{"errors":true}')
        sink = self.make("elastic", {"url": "{url}", "api_key": {"env": "K"}}, {"K": "k"})
        with self.assertRaises(DeliveryError):
            sink.send_events([failed_event()])

    def test_elastic_version_conflicts_count_as_delivered(self):
        reply = {"errors": True, "items": [{"create": {"status": 409, "error": {"type": "version_conflict_engine_exception"}}}]}
        self.server.set_default(200, json.dumps(reply).encode())
        sink = self.make("elastic", {"url": "{url}", "api_key": {"env": "K"}}, {"K": "k"})
        sink.send_events([failed_event()])

    def test_elastic_ids_are_stable_so_a_resend_creates_nothing_twice(self):
        sink = self.make("elastic", {"url": "{url}", "api_key": {"env": "K"}}, {"K": "k"})
        sink.send_events([failed_event()])
        sink.send_events([failed_event()])
        first, second = (bulk_lines(r)[0] for r in self.server.requests)
        self.assertEqual(first, second)

    def test_otlp_partial_success_is_logged_and_not_retried(self):
        self.server.set_default(200, b'{"partialSuccess":{"rejectedLogRecords":"2","errorMessage":"too old"}}')
        sink = self.make("otlp", {"endpoint": "{url}"})
        with self.assertLogs("ogun_export.sink.s1", "WARNING") as logs:
            sink.send_events([failed_event()])
        self.assertIn("rejected 2", logs.output[0])
        self.assertNotIn("too old", logs.output[0])
        self.assertEqual(len(self.server.requests), 1)

    # ---- slack ----

    def test_slack_4xx_other_than_429_is_a_failure_and_is_not_retried(self):
        hook = f"/services/T0/B0/{canary()}"
        self.server.set_default(400, b"invalid_payload")
        sink = self.make("slack", {"webhook_url": {"env": "U"}}, {"U": self.server.url + hook})
        with self.assertRaises(DeliveryError) as ctx:
            sink.send_events([failed_event()])
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(ctx.exception.status, 400)
        self.assertIn("HTTP 400", str(ctx.exception))
        self.assertNotIn(hook.rsplit("/", 1)[1], str(ctx.exception))

    def test_slack_429_waits_for_retry_after_then_sends(self):
        self.server.queue(429, b"rate_limited", {"Retry-After": "3"})
        self.server.set_default(200, b"ok")
        waits = []
        sink = self.make("slack", {"webhook_url": {"env": "U"}}, {"U": self.server.url + "/services/T/B/x"})
        sink.sleep = waits.append
        sink.send_events([failed_event()])
        self.assertEqual(waits, [3.0])
        self.assertEqual(len(self.server.requests), 2)

    def test_slack_escapes_control_characters_in_text(self):
        self.server.set_default(200, b"ok")
        sink = self.make("slack", {"webhook_url": {"env": "U"}}, {"U": self.server.url + "/services/T/B/x"})
        sink.send_events([failed_event(title="<!channel> a & b", text="<https://evil.example|click>")])
        body = self.server.requests[0].json()
        self.assertEqual(body["text"], "&lt;!channel&gt; a &amp; b: &lt;https://evil.example|click&gt;")

    def test_a_failure_part_way_records_how_many_events_were_delivered(self):
        self.server.queue(200, b"ok")
        self.server.queue(400, b"no")
        sink = self.make("slack", {"webhook_url": {"env": "U"}}, {"U": self.server.url + "/services/T/B/x"})
        with self.assertRaises(DeliveryError) as ctx:
            sink.send_events([failed_event(), attention_event(), failed_event(seq=99)])
        self.assertEqual(ctx.exception.delivered, 1)
        self.assertEqual(len(self.server.requests), 2)

    # ---- pagerduty ----

    def test_pagerduty_triggers_then_resolves_with_the_same_dedup_key(self):
        sink = self.make("pagerduty", {"routing_key": {"env": "K"}}, {"K": "routing-key"})
        sink.enqueue_url = self.server.url + "/v2/enqueue"
        recovered = failed_event(kind="node_recovered", severity="info", from_state="running", to_state="succeeded")
        sink.send_events([failed_event(), recovered])
        trigger, resolve = (r.json() for r in self.server.requests)
        self.assertEqual(trigger["event_action"], "trigger")
        self.assertEqual(resolve, {"routing_key": "routing-key", "event_action": "resolve", "dedup_key": "orun_1:fetch:2026-01-05"})
        self.assertEqual(trigger["dedup_key"], resolve["dedup_key"])

    def test_pagerduty_resolve_without_a_key_is_skipped(self):
        sink = self.make("pagerduty", {"routing_key": {"env": "K"}}, {"K": "k"})
        sink.enqueue_url = self.server.url + "/v2/enqueue"
        sink.send_events([failed_event(kind="approval_resolved", severity="info", dedup_key=None)])
        self.assertEqual(self.server.requests, [])

    def test_pagerduty_regions(self):
        us = self.make("pagerduty", {"routing_key": {"env": "K"}}, {"K": "k"})
        eu = self.make("pagerduty", {"region": "eu", "routing_key": {"env": "K"}}, {"K": "k"})
        self.assertEqual(us.enqueue_url, "https://events.pagerduty.com/v2/enqueue")
        self.assertEqual(eu.enqueue_url, "https://events.eu.pagerduty.com/v2/enqueue")

    # ---- signatures that need a clock ----

    def test_dingtalk_signature_uses_the_clock_in_milliseconds(self):
        sink = self.make("dingtalk", {"webhook_url": {"env": "U"}, "secret": {"env": "S"}}, {"U": "https://oapi.dingtalk.com/robot/send?access_token=t", "S": "SEC"})
        sink.clock = lambda: 1_600_000_000.5
        url = sink.signed_url()
        digest = hmac.new(b"SEC", b"1600000000500\nSEC", hashlib.sha256).digest()
        self.assertEqual(url, "https://oapi.dingtalk.com/robot/send?access_token=t&timestamp=1600000000500&sign=" + urllib.parse.quote_plus(base64.b64encode(digest)))

    def test_webhook_signature_covers_the_exact_bytes_sent(self):
        sink = self.make("webhook", {"url": "{url}/in", "signing_secret": {"env": "S"}}, {"S": "shh"})
        sink.send_snapshot(sample_snapshot())
        r = self.server.requests[0]
        self.assertEqual(r.header("x-ogun-signature"), webhook_signature("shh", r.body))
        self.assertNotEqual(r.header("x-ogun-signature"), webhook_signature("other", r.body))

    def test_webhook_without_a_secret_sends_no_signature(self):
        sink = self.make("webhook", {"url": "{url}/in"})
        sink.send_lineage(LINEAGE)
        self.assertIsNone(self.server.requests[0].header("x-ogun-signature"))

    def test_webhook_splits_large_batches(self):
        sink = self.make("webhook", {"url": "{url}/in"})
        sink.send_events([failed_event(seq=i) for i in range(250)])
        sizes = [len(r.json()["items"]) for r in self.server.requests]
        self.assertEqual(sizes, [100, 100, 50])

    # ---- openlineage ----

    def test_openlineage_path_is_configurable_and_auth_is_optional(self):
        sink = self.make("openlineage", {"url": "{url}/", "path": "/custom/v2/lineage"})
        sink.send_lineage(LINEAGE[:1])
        r = self.server.requests[0]
        self.assertEqual(r.route, "/custom/v2/lineage")
        self.assertIsNone(r.header("authorization"))

    def test_openlineage_default_path_is_marquez(self):
        sink = self.make("openlineage", {"url": "{url}"})
        sink.send_lineage(LINEAGE[:1])
        self.assertEqual(self.server.requests[0].route, "/api/v1/lineage")

    def test_openlineage_skips_items_that_are_not_objects(self):
        sink = self.make("openlineage", {"url": "{url}"})
        sink.send_lineage([LINEAGE[0], "junk", None, LINEAGE[1]])
        self.assertEqual(len(self.server.requests), 2)

    def test_openlineage_path_cannot_carry_a_query(self):
        with self.assertRaises(ConfigError):
            self.make("openlineage", {"url": "{url}", "path": "/lineage?token=x"})

    # ---- loki ----

    def test_loki_basic_auth_and_stream_grouping(self):
        self.server.set_default(204)
        sink = self.make("loki", {"url": "{url}", "username": {"env": "U"}, "password": {"env": "P"}}, {"U": "123", "P": "pw"})
        late = failed_event(at="2026-10-24T12:00:09.000Z", seq=2)
        early = failed_event(at="2026-10-24T12:00:01.000Z", seq=1)
        other = failed_event(workflow=None, severity="info", kind="run_finished")
        sink.send_events([late, early, other])
        r = self.server.requests[0]
        self.assertEqual(r.header("authorization"), "Basic " + base64.b64encode(b"123:pw").decode())
        self.assertIsNone(r.header("x-scope-orgid"))
        streams = r.json()["streams"]
        self.assertEqual(len(streams), 2)
        errors = [s for s in streams if s["stream"]["level"] == "error"][0]
        self.assertEqual([json.loads(v[1])["seq"] for v in errors["values"]], [1, 2])
        self.assertEqual([s["stream"]["workflow"] for s in streams if s["stream"]["level"] == "info"], ["unknown"])

    def test_prometheus_job_and_instance_are_escaped_in_the_path(self):
        sink = self.make("prometheus", {"url": "{url}", "job": "ogun_prod"})
        sink.send_snapshot(sample_snapshot())
        self.assertEqual(self.server.requests[0].route, "/metrics/job/ogun_prod")
        with self.assertRaises(ConfigError):
            self.make("prometheus", {"url": "{url}", "job": "a/b"})

    def test_datadog_truncates_to_the_documented_limits(self):
        self.server.set_default(202)
        sink = self.make("datadog", {"api_key": {"env": "K"}}, {"K": "k"})
        sink.base_url = self.server.url
        sink.send_events([failed_event(title="t" * 500, text="x" * 9000, dedup_key="d" * 300)])
        body = self.server.requests[0].json()
        self.assertEqual((len(body["title"]), len(body["text"]), len(body["aggregation_key"])), (100, 4000, 100))

    def test_datadog_event_without_a_dedup_key_has_no_aggregation_key(self):
        self.server.set_default(202)
        sink = self.make("datadog", {"api_key": {"env": "K"}}, {"K": "k"})
        sink.base_url = self.server.url
        sink.send_events([failed_event(dedup_key=None, kind="run_finished", to_state="succeeded", severity="info")])
        body = self.server.requests[0].json()
        self.assertNotIn("aggregation_key", body)
        self.assertEqual(body["alert_type"], "success")


if __name__ == "__main__":
    unittest.main()
