"""A small OpenID Connect provider for the acceptance run of native hub sign-in (O-13 section 4.3).

It speaks what Dex speaks to a public client: discovery, a key set, an authorization endpoint with a login page
(authorization code with PKCE S256, loopback redirect on any port for the public client), a token endpoint for the
authorization code and refresh token grants, and ES256 ID tokens. It checks what a real provider checks (PKCE
verifier, exact client, loopback redirect, a code works once, a refresh token can be revoked) so the app's flow is
exercised honestly. It is a stand-in for Dex, which the hosted hub uses; it is not Dex.

    python idp_mock.py --port 18700 [--lifetime 3600]

Test hooks (loopback only): POST /admin/revoke-refresh  /admin/short-lived?seconds=N
"""

import argparse
import base64
import hashlib
import html
import json
import secrets
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature

KEY = ec.generate_private_key(ec.SECP256R1())
NUMBERS = KEY.public_key().public_numbers()
KID = "acc-1"
CLIENTS = {
    "ogun-web": {"secret": "web-secret-for-acceptance-only", "public": False},
    "ogun-studio": {"secret": None, "public": True},
}
USERS = {
    "alice@example.com": {"password": "alice-password", "sub": "alice", "name": "Alice Example"},
    "bob@example.com": {"password": "bob-password", "sub": "bob", "name": "Bob Example"},
    # Signs in at the provider but has no account on the hub.
    "mallory@example.com": {"password": "mallory-password", "sub": "mallory", "name": "Mallory Example"},
}
CODES = {}
REFRESH = {}
STATE = {"lifetime": 3600, "issuer": ""}
LOG = []


def b64(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def sign(claims):
    header = {"alg": "ES256", "kid": KID, "typ": "JWT"}
    signing_input = b64(json.dumps(header).encode()) + "." + b64(json.dumps(claims).encode())
    der = KEY.sign(signing_input.encode(), ec.ECDSA(hashes.SHA256()))
    r, s = decode_dss_signature(der)
    return signing_input + "." + b64(r.to_bytes(32, "big") + s.to_bytes(32, "big"))


def id_token(user, client, nonce):
    now = int(time.time())
    claims = {
        "iss": STATE["issuer"],
        "sub": user["sub"],
        "aud": client,
        "iat": now,
        "exp": now + STATE["lifetime"],
        "email": [e for e, u in USERS.items() if u is user][0],
        "email_verified": True,
        "name": user["name"],
    }
    if nonce:
        claims["nonce"] = nonce
    return sign(claims)


def loopback_redirect(uri):
    u = urllib.parse.urlparse(uri)
    return u.scheme == "http" and u.hostname in ("127.0.0.1", "localhost", "::1") and u.path == "/callback"


class Handler(BaseHTTPRequestHandler):
    server_version = "idp-mock"

    def log_message(self, fmt, *args):
        LOG.append(fmt % args)

    def send(self, status, body, ctype="application/json", headers=None):
        data = body if isinstance(body, bytes) else body.encode()
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def json(self, status, obj):
        self.send(status, json.dumps(obj))

    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(u.query))
        base = STATE["issuer"]
        if u.path == "/.well-known/openid-configuration":
            return self.json(200, {
                "issuer": base,
                "authorization_endpoint": base + "/auth",
                "token_endpoint": base + "/token",
                "jwks_uri": base + "/keys",
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["ES256"],
                "code_challenge_methods_supported": ["S256"],
                "scopes_supported": ["openid", "email", "profile", "offline_access"],
                "token_endpoint_auth_methods_supported": ["client_secret_basic", "none"],
            })
        if u.path == "/keys":
            return self.json(200, {"keys": [{
                "kty": "EC", "crv": "P-256", "kid": KID, "alg": "ES256", "use": "sig",
                "x": b64(NUMBERS.x.to_bytes(32, "big")), "y": b64(NUMBERS.y.to_bytes(32, "big")),
            }]})
        if u.path == "/auth":
            client = CLIENTS.get(q.get("client_id", ""))
            if not client or q.get("response_type") != "code" or q.get("code_challenge_method") != "S256" or not q.get("code_challenge"):
                return self.send(400, "bad authorization request", "text/plain")
            redirect = q.get("redirect_uri", "")
            ok = loopback_redirect(redirect) if client["public"] else redirect.startswith(STATE["issuer"].rsplit(":", 1)[0])
            if not ok:
                return self.send(400, "redirect_uri is not allowed for this client", "text/plain")
            hidden = "".join(
                f'<input type="hidden" name="{html.escape(k)}" value="{html.escape(v)}">' for k, v in q.items()
            )
            page = (
                "<!doctype html><html><head><title>Sign in to the acceptance directory</title></head><body>"
                "<h1>Sign in to the acceptance directory</h1>"
                f'<form method="post" action="/login">{hidden}'
                '<label>Email <input name="login" autofocus></label> <label>Password <input name="password" type="password"></label>'
                '<button type="submit">Log in</button></form></body></html>'
            )
            return self.send(200, page, "text/html; charset=utf-8")
        self.send(404, "not found", "text/plain")

    def read_form(self):
        n = int(self.headers.get("Content-Length") or 0)
        return dict(urllib.parse.parse_qsl(self.rfile.read(n).decode()))

    def do_POST(self):
        u = urllib.parse.urlparse(self.path)
        if u.path == "/login":
            f = self.read_form()
            user = USERS.get(f.get("login", ""))
            if not user or user["password"] != f.get("password"):
                return self.send(401, "wrong email or password", "text/plain")
            code = secrets.token_urlsafe(24)
            CODES[code] = {
                "user": user, "client": f["client_id"], "redirect": f["redirect_uri"],
                "challenge": f["code_challenge"], "nonce": f.get("nonce"), "at": time.time(),
                "scope": f.get("scope", ""),
            }
            target = f["redirect_uri"] + "?" + urllib.parse.urlencode({"code": code, "state": f.get("state", "")})
            return self.send(302, "", "text/plain", {"Location": target})
        if u.path == "/token":
            f = self.read_form()
            client = CLIENTS.get(f.get("client_id", ""))
            auth = self.headers.get("Authorization", "")
            if auth.startswith("Basic "):
                cid, _, sec = base64.b64decode(auth[6:]).decode().partition(":")
                client = CLIENTS.get(urllib.parse.unquote(cid))
                if not client or client["secret"] != urllib.parse.unquote(sec):
                    return self.json(401, {"error": "invalid_client"})
                f["client_id"] = urllib.parse.unquote(cid)
            elif not client or not client["public"]:
                return self.json(401, {"error": "invalid_client"})
            if f.get("grant_type") == "authorization_code":
                c = CODES.pop(f.get("code", ""), None)
                if not c or c["client"] != f["client_id"] or c["redirect"] != f.get("redirect_uri") or time.time() - c["at"] > 300:
                    return self.json(400, {"error": "invalid_grant"})
                verifier = f.get("code_verifier", "")
                if b64(hashlib.sha256(verifier.encode()).digest()) != c["challenge"]:
                    return self.json(400, {"error": "invalid_grant", "error_description": "PKCE verifier does not match"})
                out = {"access_token": secrets.token_urlsafe(16), "token_type": "bearer", "expires_in": STATE["lifetime"],
                       "id_token": id_token(c["user"], f["client_id"], c["nonce"])}
                if "offline_access" in c["scope"].split():
                    rt = secrets.token_urlsafe(24)
                    REFRESH[rt] = {"user": c["user"], "client": f["client_id"]}
                    out["refresh_token"] = rt
                return self.json(200, out)
            if f.get("grant_type") == "refresh_token":
                r = REFRESH.get(f.get("refresh_token", ""))
                if not r or r["client"] != f["client_id"]:
                    return self.json(400, {"error": "invalid_grant"})
                out = {"access_token": secrets.token_urlsafe(16), "token_type": "bearer", "expires_in": STATE["lifetime"],
                       "id_token": id_token(r["user"], f["client_id"], None)}
                # Rotation, as Dex does: the old refresh token dies with the new one.
                del REFRESH[f["refresh_token"]]
                rt = secrets.token_urlsafe(24)
                REFRESH[rt] = r
                out["refresh_token"] = rt
                return self.json(200, out)
            return self.json(400, {"error": "unsupported_grant_type"})
        if u.path == "/admin/revoke-refresh":
            REFRESH.clear()
            return self.json(200, {"revoked": True})
        if u.path == "/admin/short-lived":
            STATE["lifetime"] = int(dict(urllib.parse.parse_qsl(u.query)).get("seconds", "3600"))
            return self.json(200, {"lifetime": STATE["lifetime"]})
        if u.path == "/admin/log":
            return self.json(200, {"log": LOG[-50:]})
        self.send(404, "not found", "text/plain")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18700)
    ap.add_argument("--lifetime", type=int, default=3600)
    a = ap.parse_args()
    STATE["issuer"] = f"http://127.0.0.1:{a.port}"
    STATE["lifetime"] = a.lifetime
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    print(f"idp-mock on {STATE['issuer']}", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    sys.exit(main())
