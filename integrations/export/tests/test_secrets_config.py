import copy
import json
import os
import pickle
import tempfile
import unittest

from fakes import canary
from ogun_export import secrets as sec
from ogun_export.config import build_sinks, load_config, parse_config, required_env
from ogun_export.secrets import ConfigError, Secret
from ogun_export.sinks import REGISTRY


def base_config(**overrides):
    cfg = {
        "version": 1,
        "credential": "export",
        "sinks": [{"name": "dd", "type": "datadog", "site": "datadoghq.com", "api_key": {"env": "DD_API_KEY"}, "tags": ["env:prod"]}],
    }
    cfg.update(overrides)
    return cfg


class SecretTests(unittest.TestCase):
    def test_never_prints(self):
        s = Secret(canary())
        for text in (repr(s), str(s), f"{s}", "%s" % s, "%r" % s, format(s, ">20")):
            self.assertEqual(text, "<secret>")
        self.assertEqual(repr([s]), "[<secret>]")
        self.assertEqual(repr({"k": s}), "{'k': <secret>}")

    def test_reveal_returns_the_value(self):
        self.assertEqual(Secret("abc").reveal(), "abc")

    def test_cannot_be_serialised_or_copied(self):
        s = Secret(canary())
        with self.assertRaises(TypeError):
            json.dumps(s)
        with self.assertRaises(TypeError):
            pickle.dumps(s)
        with self.assertRaises(TypeError):
            copy.deepcopy(s)
        with self.assertRaises(TypeError):
            copy.copy(s)

    def test_must_be_a_non_empty_string(self):
        for bad in ("", None, 5):
            with self.assertRaises(ValueError):
                Secret(bad)

    def test_resolve_reads_the_named_variable(self):
        value = canary()
        got = sec.resolve({"env": "X_KEY"}, {"X_KEY": f"  {value}\n"}, "sinks[0].api_key")
        self.assertEqual(got.reveal(), value)

    def test_missing_or_empty_variable_is_named_and_no_value_is_printed(self):
        for env in ({}, {"X_KEY": ""}, {"X_KEY": "   "}):
            with self.assertRaises(ConfigError) as ctx:
                sec.resolve({"env": "X_KEY"}, env, "sinks[0].api_key")
            self.assertIn("X_KEY", str(ctx.exception))
            self.assertIn("sinks[0].api_key", str(ctx.exception))

    def test_value_with_control_characters_is_refused_without_echo(self):
        value = canary()
        with self.assertRaises(ConfigError) as ctx:
            sec.resolve({"env": "X_KEY"}, {"X_KEY": value + "\r\nX-Injected: 1"}, "p")
        self.assertIn("X_KEY", str(ctx.exception))
        self.assertNotIn(value, str(ctx.exception))

    def test_literal_is_refused_without_echo(self):
        literal = canary()
        for spec in (literal, 5, ["env"], {"env": 5}, {"env": "BAD NAME"}, {"env": "OK", "x": 1}, {}):
            with self.assertRaises(ConfigError) as ctx:
                sec.resolve(spec, {}, "sinks[0].api_key")
            self.assertEqual(str(ctx.exception), 'sinks[0].api_key: a secret must be {"env": "NAME"}')
        with self.assertRaises(ConfigError) as ctx:
            sec.resolve(literal, {}, "p")
        self.assertNotIn(literal, str(ctx.exception))

    def test_secret_looking_names(self):
        for name in ("api_key", "token", "password", "secret", "webhook_url", "routing_key", "signing_secret", "key", "x_api_key", "bot_token"):
            self.assertTrue(sec.looks_like_secret(name), name)
        for name in ("name", "url", "site", "tags", "headers_env", "region", "index"):
            self.assertFalse(sec.looks_like_secret(name), name)

    def test_env_names_walks_nested_configuration(self):
        names = sec.env_names([{"a": {"env": "ONE"}, "b": {"c": {"env": "TWO"}}}, {"d": {"env": "ONE"}}])
        self.assertEqual(names, ["ONE", "TWO"])


class ConfigTests(unittest.TestCase):
    def test_minimal_valid_configuration_with_defaults(self):
        cfg = parse_config(base_config())
        self.assertEqual((cfg.credential, cfg.interval_s, cfg.snapshot_interval_s, cfg.state_dir), ("export", 15, 60, "~/.ogun-export"))
        self.assertIsNone(cfg.ogun_home)
        self.assertIsNone(cfg.profile)
        self.assertEqual(required_env(cfg), ["DD_API_KEY"])

    def test_full_example_from_the_documentation(self):
        cfg = parse_config(
            {
                "version": 1,
                "credential": "export",
                "ogun_home": None,
                "profile": None,
                "state_dir": "~/.ogun-export",
                "interval_s": 15,
                "snapshot_interval_s": 60,
                "sinks": [{"name": "dd", "type": "datadog", "site": "datadoghq.com", "api_key": {"env": "DD_API_KEY"}, "tags": ["env:prod"], "min_severity": "warning"}],
            }
        )
        sinks = build_sinks(cfg, {"DD_API_KEY": "k"})
        self.assertEqual([s.name for s in sinks], ["dd"])
        self.assertEqual(sinks[0].min_severity, "warning")
        self.assertEqual(sinks[0].tags, ["env:prod"])

    def test_unknown_top_level_key_is_named(self):
        with self.assertRaises(ConfigError) as ctx:
            parse_config(base_config(surprise=1))
        self.assertIn("surprise", str(ctx.exception))

    def test_unknown_sink_key_is_named(self):
        cfg = base_config()
        cfg["sinks"][0]["colour"] = "blue"
        with self.assertRaises(ConfigError) as ctx:
            parse_config(cfg)
        self.assertIn("colour", str(ctx.exception))
        self.assertIn("sinks[0]", str(ctx.exception))

    def test_unknown_key_that_looks_like_a_secret_never_echoes_its_value(self):
        literal = canary()
        cfg = base_config()
        cfg["sinks"][0]["password"] = literal
        with self.assertRaises(ConfigError) as ctx:
            parse_config(cfg)
        self.assertNotIn(literal, str(ctx.exception))

    def test_literal_secret_is_refused_without_echoing_it(self):
        literal = canary()
        cfg = base_config()
        cfg["sinks"][0]["api_key"] = literal
        with self.assertRaises(ConfigError) as ctx:
            parse_config(cfg)
        self.assertEqual(str(ctx.exception), 'sinks[0].api_key: a secret must be {"env": "NAME"}')
        self.assertNotIn(literal, str(ctx.exception))
        self.assertNotIn(literal, repr(ctx.exception))

    def test_literal_webhook_url_is_refused_without_echoing_it(self):
        literal = "https://hooks.slack.com/services/T0/B0/" + canary()
        cfg = base_config(sinks=[{"name": "chat", "type": "slack", "webhook_url": literal}])
        with self.assertRaises(ConfigError) as ctx:
            parse_config(cfg)
        self.assertIn("a secret must be", str(ctx.exception))
        self.assertNotIn("OGUN-CANARY", str(ctx.exception))

    def test_missing_variable_is_named_without_any_value(self):
        cfg = parse_config(base_config())
        with self.assertRaises(ConfigError) as ctx:
            build_sinks(cfg, {"UNRELATED": canary()})
        self.assertIn("DD_API_KEY", str(ctx.exception))
        self.assertNotIn("OGUN-CANARY", str(ctx.exception))

    def test_duplicate_sink_names_are_refused(self):
        s = base_config()["sinks"][0]
        with self.assertRaises(ConfigError) as ctx:
            parse_config(base_config(sinks=[s, dict(s)]))
        self.assertIn("used twice", str(ctx.exception))

    def test_sink_names_follow_the_pattern(self):
        for bad in ("", "Dd", "1dd", "d d", "x" * 33, "dd/../x", None, 5):
            cfg = base_config()
            cfg["sinks"][0]["name"] = bad
            with self.assertRaises(ConfigError, msg=repr(bad)):
                parse_config(cfg)
        cfg = base_config()
        cfg["sinks"][0]["name"] = "a" + "b0_-" * 7 + "z"
        parse_config(cfg)

    def test_other_structural_errors(self):
        bad = [
            [],
            {"sinks": []},
            base_config(version=2),
            base_config(version=True),
            base_config(sinks=[]),
            base_config(sinks="x"),
            base_config(sinks=["x"]),
            base_config(credential="../etc"),
            base_config(interval_s=0),
            base_config(interval_s="15"),
            base_config(snapshot_interval_s=True),
            base_config(state_dir=""),
            base_config(sinks=[{"name": "a", "type": "nonesuch"}]),
            base_config(sinks=[{"name": "a"}]),
        ]
        for cfg in bad:
            with self.subTest(cfg=str(cfg)[:60]):
                with self.assertRaises(ConfigError):
                    parse_config(cfg)

    def test_sink_values_are_validated(self):
        def sink(**kw):
            s = {"name": "dd", "type": "datadog", "api_key": {"env": "K"}}
            s.update(kw)
            return base_config(sinks=[s])

        for kw in ({"site": "example.com"}, {"min_severity": "attention"}, {"min_severity": "loud"}, {"tags": "env:prod"}, {"tags": [1]}, {"timeout_s": 0}, {"timeout_s": 500}):
            with self.subTest(kw=kw), self.assertRaises(ConfigError):
                parse_config(sink(**kw))

    def test_urls_must_not_carry_credentials_or_a_query(self):
        for url in ("https://user:pw@example.com/x", "https://example.com/x?token=abc", "https://example.com/x#frag", "ftp://example.com/x", "example.com/x"):
            with self.subTest(url=url):
                cfg = base_config(sinks=[{"name": "w", "type": "webhook", "url": url}])
                with self.assertRaises(ConfigError) as ctx:
                    parse_config(cfg)
                self.assertNotIn("pw", str(ctx.exception))
                self.assertNotIn("abc", str(ctx.exception))

    def test_plain_http_needs_loopback_or_the_override(self):
        remote = base_config(sinks=[{"name": "w", "type": "webhook", "url": "http://collector.example.com/in"}])
        with self.assertRaises(ConfigError) as ctx:
            parse_config(remote)
        self.assertIn("allow_insecure_http", str(ctx.exception))
        remote["sinks"][0]["allow_insecure_http"] = True
        parse_config(remote)
        parse_config(base_config(sinks=[{"name": "w", "type": "webhook", "url": "http://127.0.0.1:9000/in"}]))

    def test_secret_url_from_the_environment_is_checked_when_built(self):
        cfg = parse_config(base_config(sinks=[{"name": "chat", "type": "slack", "webhook_url": {"env": "HOOK"}}]))
        literal = "http://example.com/hook/" + canary()
        with self.assertRaises(ConfigError) as ctx:
            build_sinks(cfg, {"HOOK": literal})
        self.assertNotIn("OGUN-CANARY", str(ctx.exception))
        self.assertIn("HOOK", required_env(cfg))
        self.assertEqual(len(build_sinks(cfg, {"HOOK": "https://hooks.slack.com/services/T/B/x"})), 1)

    def test_cross_checks(self):
        for sink in (
            {"name": "o", "type": "openlineage", "url": "https://m.example.com", "header_name": "X-Token"},
            {"name": "p", "type": "prometheus", "url": "https://g.example.com", "username": {"env": "U"}},
            {"name": "l", "type": "loki", "url": "https://l.example.com", "token": {"env": "T"}, "username": {"env": "U"}, "password": {"env": "P"}},
            {"name": "w", "type": "webhook", "url": "https://w.example.com", "headers_env": {"X-Ogun-Signature": {"env": "S"}}},
            {"name": "w", "type": "webhook", "url": "https://w.example.com", "headers_env": {"X-Token": "literal"}},
        ):
            with self.subTest(sink=sink["type"]), self.assertRaises(ConfigError):
                parse_config(base_config(sinks=[sink]))

    def test_pagerduty_takes_no_min_severity(self):
        with self.assertRaises(ConfigError):
            parse_config(base_config(sinks=[{"name": "pd", "type": "pagerduty", "routing_key": {"env": "K"}, "min_severity": "info"}]))

    def test_load_config_reports_unreadable_and_invalid_files_without_content(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(ConfigError):
                load_config(os.path.join(d, "missing.json"))
            bad = os.path.join(d, "bad.json")
            literal = canary()
            with open(bad, "w", encoding="utf-8") as f:
                f.write('{"version": 1, "token": "' + literal + '" oops')
            with self.assertRaises(ConfigError) as ctx:
                load_config(bad)
            self.assertNotIn(literal, str(ctx.exception))
            good = os.path.join(d, "good.json")
            with open(good, "w", encoding="utf-8") as f:
                json.dump(base_config(), f)
            self.assertEqual(load_config(good).sinks[0]["name"], "dd")

    def test_every_sink_type_has_a_complete_catalogue_entry(self):
        for name, cls in REGISTRY.items():
            entry = cls.catalogue()
            self.assertEqual(entry["type"], name)
            self.assertTrue(entry["description"])
            self.assertTrue(entry["capabilities"])
            self.assertTrue(entry["egress"])
            self.assertTrue(set(entry["capabilities"]) <= {"events", "metrics", "lineage"})


if __name__ == "__main__":
    unittest.main()
