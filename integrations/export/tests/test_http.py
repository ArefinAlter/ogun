import unittest
import urllib.error
from unittest import mock

from fakes import FakeServer, canary, noop_sleep
from ogun_export import http
from ogun_export.http import DeliveryError


class RedactTests(unittest.TestCase):
    def test_removes_userinfo_query_and_fragment(self):
        shown = http.redact_url("https://user:pw@example.com:8443/a/b?token=SECRET#frag")
        self.assertEqual(shown, "https://example.com:8443/a/b")

    def test_can_hide_the_path_too(self):
        self.assertEqual(http.redact_url("https://hooks.example.com/services/T/B/SECRET?x=1", keep_path=False), "https://hooks.example.com/...")

    def test_garbage_is_not_echoed(self):
        self.assertEqual(http.redact_url("not a url with SECRET"), "<invalid url>")

    def test_only_http_and_https_and_plain_http_only_on_loopback(self):
        self.assertIsNotNone(http.url_problem("ftp://example.com/x"))
        self.assertIsNotNone(http.url_problem("http://example.com/x"))
        self.assertIsNone(http.url_problem("http://example.com/x", allow_insecure_http=True))
        self.assertIsNone(http.url_problem("http://127.0.0.1:9/x"))
        self.assertIsNone(http.url_problem("http://localhost/x"))
        self.assertIsNone(http.url_problem("http://[::1]:80/x"))
        self.assertIsNone(http.url_problem("https://example.com/x"))
        self.assertIsNotNone(http.url_problem("https:///nohost"))


class PostTests(unittest.TestCase):
    def setUp(self):
        self.server = FakeServer().start()
        self.addCleanup(self.server.stop)
        self.sleeps = []

    def post(self, body=b"{}", path="/p", **kw):
        return http.post(
            self.server.url + path,
            body,
            kw.pop("headers", {"Content-Type": "application/json"}),
            5,
            sink="demo",
            sleep=self.sleeps.append,
            **kw,
        )

    def test_success_returns_the_response_body_and_sends_what_it_was_given(self):
        self.server.set_default(200, b"hello")
        out = self.post(b'{"a":1}', headers={"Content-Type": "application/json", "X-Test": "yes"})
        self.assertEqual(out, b"hello")
        req = self.server.requests[0]
        self.assertEqual((req.method, req.path, req.body), ("POST", "/p", b'{"a":1}'))
        self.assertEqual(req.header("x-test"), "yes")
        self.assertTrue(req.header("user-agent").startswith("ogun-export/"))

    def test_put_method(self):
        self.post(method="PUT")
        self.assertEqual(self.server.requests[0].method, "PUT")

    def test_5xx_is_retried_then_succeeds_with_exponential_backoff(self):
        self.server.queue(503)
        self.server.queue(500)
        self.server.set_default(200, b"ok")
        self.assertEqual(self.post(), b"ok")
        self.assertEqual(len(self.server.requests), 3)
        self.assertEqual(self.sleeps, [0.5, 1.0])

    def test_gives_up_after_three_retries(self):
        self.server.set_default(502)
        with self.assertRaises(DeliveryError) as ctx:
            self.post()
        self.assertEqual(len(self.server.requests), 4)
        self.assertEqual(self.sleeps, [0.5, 1.0, 2.0])
        self.assertEqual(ctx.exception.status, 502)

    def test_4xx_is_not_retried(self):
        self.server.set_default(400, b"bad request")
        with self.assertRaises(DeliveryError) as ctx:
            self.post()
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(self.sleeps, [])
        self.assertEqual(ctx.exception.status, 400)

    def test_404_and_401_are_not_retried(self):
        for status in (401, 403, 404, 422):
            with self.subTest(status=status):
                self.server.requests.clear()
                self.server.set_default(status)
                with self.assertRaises(DeliveryError):
                    self.post()
                self.assertEqual(len(self.server.requests), 1)

    def test_429_honours_retry_after(self):
        self.server.queue(429, headers={"Retry-After": "2"})
        self.server.set_default(200)
        self.post()
        self.assertEqual(self.sleeps, [2.0])
        self.assertEqual(len(self.server.requests), 2)

    def test_retry_after_is_capped(self):
        self.server.queue(429, headers={"Retry-After": "86400"})
        self.post()
        self.assertEqual(self.sleeps, [60.0])

    def test_429_without_retry_after_uses_backoff(self):
        self.server.queue(429)
        self.post()
        self.assertEqual(self.sleeps, [0.5])

    def test_dropped_connection_is_retried(self):
        self.server.queue(0)
        self.server.queue(0)
        self.server.set_default(200, b"fine")
        self.assertEqual(self.post(), b"fine")
        self.assertEqual(len(self.server.requests), 3)

    def test_redirects_are_not_followed(self):
        target = FakeServer().start()
        self.addCleanup(target.stop)
        self.server.set_default(302, headers={"Location": target.url + "/elsewhere"})
        with self.assertRaises(DeliveryError) as ctx:
            self.post(headers={"Authorization": "Bearer " + canary()})
        self.assertEqual(target.requests, [])
        self.assertEqual(len(self.server.requests), 1)
        self.assertIn("HTTP 302", str(ctx.exception))

    def test_error_message_has_status_sink_and_redacted_url_only(self):
        secret = canary()
        self.server.set_default(500, ("response body " + secret).encode())
        with self.assertRaises(DeliveryError) as ctx:
            http.post(
                f"{self.server.url}/hook/path?token={secret}#{secret}",
                b'{"body":"' + secret.encode() + b'"}',
                {"Authorization": "Bearer " + secret},
                5,
                sink="demo",
                sleep=noop_sleep,
            )
        message = str(ctx.exception)
        self.assertIn("HTTP 500", message)
        self.assertIn('"demo"', message)
        self.assertIn(self.server.url + "/hook/path", message)
        self.assertNotIn(secret, message)
        self.assertNotIn(secret, repr(ctx.exception))
        self.assertIsNone(ctx.exception.__context__)
        self.assertNotIn("?", message)

    def test_secret_url_hides_the_path(self):
        secret = canary()
        self.server.set_default(400)
        with self.assertRaises(DeliveryError) as ctx:
            http.post(f"{self.server.url}/services/{secret}", b"{}", {}, 5, sink="demo", sleep=noop_sleep, secret_url=True)
        self.assertNotIn(secret, str(ctx.exception))
        self.assertIn(self.server.url + "/...", str(ctx.exception))

    def test_refuses_plain_http_to_a_remote_host_without_sending(self):
        with self.assertRaises(DeliveryError) as ctx:
            http.post("http://example.com/x?k=" + canary(), b"{}", {}, 5, sink="demo", sleep=noop_sleep)
        self.assertIn("plain http", str(ctx.exception))
        self.assertNotIn("OGUN-CANARY", str(ctx.exception))

    def test_connection_failure_message_names_only_the_error_type(self):
        class Refusing:
            calls = 0

            def open(self, req, timeout=None):
                Refusing.calls += 1
                raise urllib.error.URLError(ConnectionRefusedError("refused for " + req.full_url))

        with mock.patch.object(http, "_opener", lambda url: Refusing()):
            with self.assertRaises(DeliveryError) as ctx:
                http.post("http://127.0.0.1:9/x?k=" + canary(), b"{}", {}, 1, sink="demo", sleep=self.sleeps.append)
        self.assertEqual(Refusing.calls, 4)
        self.assertEqual(self.sleeps, [0.5, 1.0, 2.0])
        message = str(ctx.exception)
        self.assertIn("connection error (ConnectionRefusedError)", message)
        self.assertIn('"demo"', message)
        self.assertNotIn("OGUN-CANARY", message)
        self.assertNotIn("refused for", message)

    def test_header_value_with_a_newline_is_rejected_without_echoing_it(self):
        secret = canary()
        with self.assertRaises(DeliveryError) as ctx:
            self.post(headers={"X-Bad": secret + "\r\nInjected: 1"})
        self.assertNotIn(secret, str(ctx.exception))
        self.assertEqual(self.server.requests, [])


if __name__ == "__main__":
    unittest.main()
