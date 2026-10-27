"""Local fixture servers for the standalone acceptance run (loopback only, dual stack).

    python fixture_server.py --port 38417

What it serves, all on 127.0.0.1 and ::1 (`fixture.localtest.me` resolves to both; `*.localhost` does not resolve
on Windows):

  /robots.txt                 404 (so `acquire` may fetch)
  /releases.json              a releases-like JSON array
  /daily/<YYYY-MM-DD>.json    one small JSON document per day, after a short delay; every hit is counted
  /stats                      hits per path, as JSON (the proof of what ran again after a kill)
  /<bucket>/<key>             an S3-compatible store (path style, bucket `acc`) that verifies AWS Signature V4
  /s3-admin/objects           what the S3 store holds
  /v1/chat/completions        an OpenAI-compatible streaming endpoint that speaks the real wire format, with tool
                              calls, and records what it was sent (never the key itself)
  /admin/requests             the recorded chat requests
"""

import argparse
import hashlib
import hmac
import json
import socket
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HITS = {}
OBJECTS = {}
CHAT = []
LOCK = threading.Lock()
S3_ACCESS_KEY = "ACCEPTANCEKEYID"
S3_SECRET_KEY = "acceptance-secret-ZEUS-CANARY-s3-0123456789"
CHAT_KEY = "acceptance-chat-key-ZEUS-CANARY-7f3a"
DELAY_MS = 250


def hit(path):
    with LOCK:
        HITS[path] = HITS.get(path, 0) + 1


def hmac_sha256(key, msg):
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()


def s3_signature(method, path, query, headers, signed_headers, payload_hash, amz_date, scope):
    canonical_headers = "".join(f"{h}:{' '.join(headers.get(h, '').split())}\n" for h in signed_headers)
    canonical_query = "&".join(
        f"{urllib.parse.quote(k, safe='-_.~')}={urllib.parse.quote(v, safe='-_.~')}"
        for k, v in sorted(urllib.parse.parse_qsl(query, keep_blank_values=True))
    )
    canonical_uri = urllib.parse.quote(urllib.parse.unquote(path), safe="/-_.~")
    request = "\n".join([method, canonical_uri, canonical_query, canonical_headers, ";".join(signed_headers), payload_hash])
    date, region, service, _ = scope.split("/")
    to_sign = "\n".join(["AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(request.encode()).hexdigest()])
    k = hmac_sha256(("AWS4" + S3_SECRET_KEY).encode(), date)
    k = hmac.new(k, region.encode(), hashlib.sha256).digest()
    k = hmac.new(k, service.encode(), hashlib.sha256).digest()
    k = hmac.new(k, b"aws4_request", hashlib.sha256).digest()
    return hmac.new(k, to_sign.encode(), hashlib.sha256).hexdigest()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def reply(self, status, body=b"", ctype="application/json", headers=None):
        if isinstance(body, str):
            body = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    # ---- S3
    def s3(self, method, parsed):
        raw = self.body() if method in ("PUT", "POST") else b""
        auth = self.headers.get("Authorization", "")
        if not auth.startswith("AWS4-HMAC-SHA256 "):
            return self.reply(403, "<Error><Code>AccessDenied</Code></Error>", "application/xml")
        parts = dict(p.strip().split("=", 1) for p in auth[len("AWS4-HMAC-SHA256 "):].split(","))
        cred = parts["Credential"].split("/", 1)
        signed = parts["SignedHeaders"].split(";")
        headers = {k.lower(): v for k, v in self.headers.items()}
        amz_date = headers.get("x-amz-date", "")
        payload_hash = headers.get("x-amz-content-sha256", hashlib.sha256(raw).hexdigest())
        if cred[0] != S3_ACCESS_KEY:
            return self.reply(403, "<Error><Code>InvalidAccessKeyId</Code></Error>", "application/xml")
        want = s3_signature(method, parsed.path, parsed.query, headers, signed, payload_hash, amz_date, cred[1])
        if not hmac.compare_digest(want, parts["Signature"]):
            hit("s3-bad-signature")
            return self.reply(403, "<Error><Code>SignatureDoesNotMatch</Code></Error>", "application/xml")
        if payload_hash not in ("UNSIGNED-PAYLOAD",) and not payload_hash.startswith("STREAMING") and hashlib.sha256(raw).hexdigest() != payload_hash:
            return self.reply(400, "<Error><Code>XAmzContentSHA256Mismatch</Code></Error>", "application/xml")
        key = parsed.path.lstrip("/")
        if key.startswith("s3/"):
            key = key[3:]
        if method == "PUT":
            with LOCK:
                OBJECTS[key] = raw
            hit("s3-put")
            return self.reply(200, b"", "application/xml", {"ETag": '"' + hashlib.md5(raw).hexdigest() + '"'})
        if method == "DELETE":
            with LOCK:
                OBJECTS.pop(key, None)
            hit("s3-delete")
            return self.reply(204, b"", "application/xml")
        if method in ("GET", "HEAD"):
            q = dict(urllib.parse.parse_qsl(parsed.query))
            if key.count("/") == 0 and ("list-type" in q or key.endswith("/") or "/" not in key):
                prefix = q.get("prefix", "")
                items = "".join(
                    f"<Contents><Key>{k.split('/', 1)[1]}</Key><Size>{len(v)}</Size></Contents>"
                    for k, v in sorted(OBJECTS.items()) if k.startswith(key + "/" + prefix)
                )
                return self.reply(200, f"<ListBucketResult><Name>{key}</Name>{items}</ListBucketResult>", "application/xml")
            obj = OBJECTS.get(key)
            if obj is None:
                return self.reply(404, "<Error><Code>NoSuchKey</Code></Error>", "application/xml")
            return self.reply(200, obj, "application/octet-stream")
        return self.reply(405)

    # ---- chat
    def chat(self):
        def tool_call(name, args, pin, pout):
            return [
                {"choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}}]},
                {"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": name, "arguments": ""}}]}}]},
                {"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": args}}]}}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": pin, "completion_tokens": pout, "total_tokens": pin + pout}},
            ]

        raw = self.body()
        body = json.loads(raw or b"{}")
        authorized = self.headers.get("Authorization", "") == f"Bearer {CHAT_KEY}"
        with LOCK:
            CHAT.append({
                "authorized": authorized,
                "model": body.get("model"),
                "stream": body.get("stream"),
                "messages": body.get("messages"),
                "tools": [t.get("function", {}).get("name") for t in body.get("tools", [])],
                "has_key_in_body": CHAT_KEY.encode() in raw,
            })
        if not authorized:
            return self.reply(401, json.dumps({"error": {"message": "invalid key", "type": "invalid_request_error"}}))
        msgs = body.get("messages", [])
        last = msgs[-1] if msgs else {}
        user_text = next((m.get("content", "") for m in reversed(msgs) if m.get("role") == "user"), "")
        tool_names = [t["function"]["name"] for t in body.get("tools", [])]
        chunks = []
        if last.get("role") == "tool":
            result = str(last.get("content", ""))[:200]
            text = "Here is what I found: " + result
            for i in range(0, len(text), 24):
                chunks.append({"choices": [{"index": 0, "delta": {"content": text[i:i + 24]}}]})
            chunks.append({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 120, "completion_tokens": 30, "total_tokens": 150}})
        elif "list workflows" in user_text.lower() and "workflows__list" in tool_names:
            chunks += tool_call("workflows__list", "{}", 90, 12)
        elif "start a run" in user_text.lower():
            # The model asks for a tool even if the thread's access level will not allow it.
            chunks += tool_call("runs__start", json.dumps({"workflow": "acc-local-fixture", "partitions": {"kind": "latest"}}), 100, 20)
        elif "approve everything" in user_text.lower():
            # A tool that is not in the closed table: whatever a model invents, nothing decides an approval.
            chunks += tool_call("approvals__decide", json.dumps({"approval_id": "oap_none", "decision": "approve"}), 80, 14)
        else:
            text = "I can list your workflows, show runs and propose a run for you to press."
            chunks.append({"choices": [{"index": 0, "delta": {"role": "assistant", "content": text}}]})
            chunks.append({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 60, "completion_tokens": 16, "total_tokens": 76}})
        out = "".join("data: " + json.dumps(c) + "\n\n" for c in chunks) + "data: [DONE]\n\n"
        return self.reply(200, out, "text/event-stream")

    def route(self, method):
        parsed = urllib.parse.urlparse(self.path)
        p = parsed.path
        if p == "/acc" or p.startswith("/acc/") or p.startswith("/s3/"):
            return self.s3(method, parsed)
        if p == "/robots.txt":
            return self.reply(404, "no robots", "text/plain")
        if p == "/releases.json":
            hit(p)
            rel = [
                {"id": 1000 + i, "tag_name": f"v0.0.{i}", "name": f"Release {i}", "draft": i == 7,
                 "prerelease": i % 3 == 0, "published_at": f"2026-0{1 + i % 9}-1{i % 9}T10:00:00Z"}
                for i in range(1, 11)
            ]
            return self.reply(200, json.dumps(rel))
        if p.startswith("/daily/") and p.endswith(".json"):
            day = p[len("/daily/"):-5]
            hit(p)
            time.sleep(DELAY_MS / 1000)
            seed = int(hashlib.sha256(day.encode()).hexdigest()[:6], 16)
            return self.reply(200, json.dumps({"day": day, "rides": seed % 5000, "stations": [
                {"id": f"S{n}", "bikes": (seed >> n) % 20} for n in range(1, 6)]}))
        if p == "/stats":
            with LOCK:
                return self.reply(200, json.dumps(HITS))
        if p == "/s3-admin/objects":
            with LOCK:
                return self.reply(200, json.dumps({k: len(v) for k, v in OBJECTS.items()}))
        if p == "/s3-admin/get":
            with LOCK:
                return self.reply(200, OBJECTS.get(dict(urllib.parse.parse_qsl(parsed.query)).get("key", ""), b""), "application/octet-stream")
        if p == "/admin/requests":
            with LOCK:
                return self.reply(200, json.dumps(CHAT))
        if p == "/admin/reset":
            with LOCK:
                HITS.clear()
                CHAT.clear()
                OBJECTS.clear()
            return self.reply(200, "{}")
        if p == "/v1/chat/completions" and method == "POST":
            return self.chat()
        if p == "/v1/models":
            return self.reply(200, json.dumps({"data": [{"id": "acceptance-model"}]}))
        return self.reply(404, "not found", "text/plain")

    def do_GET(self):
        self.route("GET")

    def do_HEAD(self):
        self.route("HEAD")

    def do_PUT(self):
        self.route("PUT")

    def do_DELETE(self):
        self.route("DELETE")

    def do_POST(self):
        self.route("POST")


class V6Server(ThreadingHTTPServer):
    address_family = socket.AF_INET6
    daemon_threads = True


class V4Server(ThreadingHTTPServer):
    daemon_threads = True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=38417)
    ap.add_argument("--delay-ms", type=int, default=250)
    a = ap.parse_args()
    global DELAY_MS
    DELAY_MS = a.delay_ms
    servers = [V4Server(("127.0.0.1", a.port), Handler)]
    try:
        servers.append(V6Server(("::1", a.port), Handler))
    except OSError:
        pass
    for s in servers[1:]:
        threading.Thread(target=s.serve_forever, daemon=True).start()
    print(f"fixture on port {a.port} ({len(servers)} listeners)", flush=True)
    try:
        servers[0].serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    sys.exit(main())
