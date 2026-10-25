#!/usr/bin/env python3
"""First-time accounts and tokens of the hosted pre-alpha hub (README.md, step 6). Standard library only.

Run on the host after the stack and its nginx site are up:

    python3 tools/bootstrap.py https://ogun.103.191.50.32.sslip.io

It signs the owner in through the one-time enrolment link, invites the shared sample account as an editor and
signs it in once, and mints two API tokens (both act as agents and can never decide an approval):

    secrets/agent-token     the pre-alpha service's agent, role editor
    secrets/desktop-token   the public token the desktop release links with, role editor

Passwords are read from secrets/ and owner-login.txt; nothing secret is printed.
"""
import html.parser
import http.cookiejar
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
BASE = sys.argv[1].rstrip('/') if len(sys.argv) > 1 else 'https://ogun.103.191.50.32.sslip.io'


class Forms(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.forms, self.links = [], []

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == 'form':
            self.forms.append({'action': a.get('action', ''), 'inputs': {}})
        elif tag == 'input' and self.forms and a.get('name'):
            self.forms[-1]['inputs'][a['name']] = a.get('value', '')
        elif tag == 'a' and a.get('href'):
            self.links.append(a['href'])


class Browser:
    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.csrf = None

    def get(self, url):
        with self.op.open(urllib.request.Request(url, headers={'accept': 'text/html,application/json'}), timeout=30) as r:
            return r.geturl(), r.read().decode('utf-8', 'replace')

    def post_form(self, url, fields):
        data = urllib.parse.urlencode(fields).encode()
        with self.op.open(urllib.request.Request(url, data=data, headers={'content-type': 'application/x-www-form-urlencoded'}), timeout=30) as r:
            return r.geturl(), r.read().decode('utf-8', 'replace')

    def api(self, method, path, body=None):
        headers = {'accept': 'application/json'}
        if body is not None:
            headers['content-type'] = 'application/json'
        if method != 'GET' and self.csrf:
            headers['x-ogun-csrf'] = self.csrf
        req = urllib.request.Request(BASE + path, method=method, headers=headers,
                                     data=None if body is None else json.dumps(body).encode())
        with self.op.open(req, timeout=30) as r:
            return json.loads(r.read() or b'null')

    def sign_in(self, start_url, email, password):
        url, page = self.get(start_url)
        if '/enroll/' in url:
            p = Forms(); p.feed(page)
            link = next(h for h in p.links if h.startswith('/auth/login'))
            url, page = self.get(BASE + html.unescape(link))
        for _ in range(3):
            p = Forms(); p.feed(page)
            form = next((f for f in p.forms if 'password' in f['inputs']), None)
            if form:
                break
            # Dex's connector chooser: follow the local (password) connector.
            link = next((h for h in p.links if '/auth/local' in h), None)
            if not link:
                raise SystemExit(f'no sign-in form at {urllib.parse.urlsplit(url).path}')
            url, page = self.get(urllib.parse.urljoin(url, html.unescape(link)))
        fields = dict(form['inputs'])
        fields.update({'login': email, 'password': password})
        url, page = self.post_form(urllib.parse.urljoin(url, html.unescape(form['action']) or url), fields)
        me = self.api('GET', '/api/v1/me')
        self.csrf = me.get('csrf_token')
        return me


def secret(name):
    return (HERE / 'secrets' / name).read_text().strip()


def write_secret(name, value):
    p = HERE / 'secrets' / name
    fd = os.open(p, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, 'w') as f:
        f.write(value + '\n')
    os.chown(p, 10001, 10001)
    print(f'wrote secrets/{name} (value not shown)')


def main():
    owner_pw = re.search(r'password:\s*(\S+)', (HERE / 'owner-login.txt').read_text()).group(1)
    code_url = subprocess.run(['docker', 'compose', 'exec', '-T', 'ogun-server', 'cat', '/var/lib/ogun/ogun-server/owner-enrolment.txt'],
                              cwd=HERE, capture_output=True, text=True, check=True).stdout
    link = re.search(r'(/enroll/[A-Za-z0-9_-]+)', code_url).group(1)
    owner = Browser()
    me = owner.sign_in(BASE + link, 'owner@ogun.local', owner_pw)
    subprocess.run(['docker', 'compose', 'exec', '-T', 'ogun-server', 'rm', '-f', '/var/lib/ogun/ogun-server/owner-enrolment.txt'], cwd=HERE, check=True)
    wsp = me['workspaces'][0]['workspace_id']
    print('owner signed in; workspace', wsp, 'role', me['workspaces'][0]['role'])
    invite = owner.api('POST', f'/api/v1/workspaces/{wsp}/invites', {'role': 'editor', 'approver': False})
    sample = Browser()
    sme = sample.sign_in(invite['invite_url'] if invite['invite_url'].startswith('http') else BASE + invite['invite_url'], 'sample@example.org', secret('sample-password'))
    print('sample account signed in; role', sme['workspaces'][0]['role'])
    for name, file in (('prealpha-agent', 'agent-token'), ('studio-desktop-prealpha', 'desktop-token')):
        t = owner.api('POST', '/api/v1/tokens', {'name': name, 'role': 'editor', 'expires_in_days': 365})
        write_secret(file, t['secret'])


if __name__ == '__main__':
    main()
