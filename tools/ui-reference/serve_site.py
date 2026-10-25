#!/usr/bin/env python3
"""Serves a built static site with the response headers its vercel.json sets, so the real policy applies locally.

    python serve_site.py --dir ../ogun-site/dist --vercel ../ogun-site/vercel.json --port 4175

Only the `headers` rules with a catch-all source `/(.*)` are applied (that is where the Content-Security-Policy
lives), plus `trailingSlash` redirects for directories. It is for checking a build against its CSP, not a host.
"""
import argparse
import json
import posixpath
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument('--dir', required=True)
    ap.add_argument('--vercel', required=True)
    ap.add_argument('--port', type=int, default=4175)
    args = ap.parse_args()
    cfg = json.loads(Path(args.vercel).read_text(encoding='utf-8'))
    headers = {}
    for rule in cfg.get('headers', []):
        if rule.get('source') == '/(.*)':
            for h in rule['headers']:
                headers[h['key']] = h['value']
    # Strict-Transport-Security and upgrade-insecure-requests would force https on localhost; drop them for the check.
    headers.pop('Strict-Transport-Security', None)
    if 'Content-Security-Policy' in headers:
        headers['Content-Security-Policy'] = headers['Content-Security-Policy'].replace('; upgrade-insecure-requests', '')

    class Handler(SimpleHTTPRequestHandler):
        def end_headers(self):
            for k, v in headers.items():
                self.send_header(k, v)
            super().end_headers()

        def do_GET(self):
            path = self.translate_path(self.path.split('?')[0])
            if Path(path).is_dir() and not self.path.split('?')[0].endswith('/'):
                self.send_response(308)
                self.send_header('Location', self.path.split('?')[0] + '/')
                self.end_headers()
                return
            super().do_GET()

        def log_message(self, *a):
            pass

    ThreadingHTTPServer(('127.0.0.1', args.port), partial(Handler, directory=args.dir)).serve_forever()


if __name__ == '__main__':
    main()
