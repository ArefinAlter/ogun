#!/usr/bin/env python3
"""Signs in to a running Ogun hub (ogun-server) with headless Chrome and checks Studio-web against the real daemon.

    OGUN_HUB_PASSWORD=... python hub_check.py --base http://localhost:18443 --out .design-reference/hub
    python hub_check.py --enroll-file owner-enrolment.txt ...      # the first time: consumes the owner link
    python hub_check.py --make-token token.txt ...                  # also create a one-day viewer API token

The hub is the loopback one in deploy/demo-hub, reached through an SSH tunnel. The sign-in goes through the
provider's own form (Dex there), so this exercises OIDC with PKCE, the session, the CSRF header and Studio-web's
routes. It reports every console error and every failed request, and exits 1 if a check fails. The password, the
owner link and a token are never printed. Needs Chrome, `websockets` and `requests`.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import requests
import websockets

sys.path.insert(0, str(Path(__file__).parent))
from shoot_studio import Cdp, find_chrome  # noqa: E402

ROUTES = [
    '#/', '#/agent', '#/workflows', '#/runs', '#/partitions', '#/approvals', '#/artifacts', '#/streams', '#/edges',
    '#/settings/general', '#/settings/agent', '#/settings/appearance', '#/settings/keybindings', '#/settings/evidence', '#/settings/providers',
    '#/settings/connectors', '#/settings/connections', '#/settings/schedules', '#/settings/sources', '#/settings/storage',
    '#/settings/environments', '#/settings/security', '#/settings/archive',
]

ME_JS = (
    "fetch('/api/v1/me', {credentials: 'include'}).then(r => r.json().then(j => ({status: r.status, kind: j.kind, "
    "user: j.user && j.user.display_name, roles: (j.workspaces || []).map(w => w.role), csrf: !!j.csrf_token})))"
)
STATUS_JS = (
    "fetch('/api/v1/status', {credentials: 'include'}).then(r => r.json().then(j => ({status: r.status, "
    "profile: j.profile, version: j.daemon_version, workflows: j.workflows})))"
)
TOKEN_JS = """(async () => {
  const me = await (await fetch('/api/v1/me', {credentials: 'include'})).json();
  const h = {'content-type': 'application/json', 'x-ogun-csrf': me.csrf_token};
  const list = await (await fetch('/api/v1/tokens', {credentials: 'include'})).json();
  for (const o of (list.tokens || []).filter(x => x.name === 'desktop-check')) {
    await fetch('/api/v1/tokens/' + o.token_id, {method: 'DELETE', credentials: 'include', headers: h});
  }
  const r = await fetch('/api/v1/tokens', {method: 'POST', credentials: 'include', headers: h,
    body: JSON.stringify({name: 'desktop-check', role: 'viewer', expires_in_days: 1})});
  const j = await r.json();
  return {status: r.status, secret: j.secret || null};
})()"""

REVOKE_JS = """(async (name) => {
  const me = await (await fetch('/api/v1/me', {credentials: 'include'})).json();
  const h = {'content-type': 'application/json', 'x-ogun-csrf': me.csrf_token};
  const list = await (await fetch('/api/v1/tokens', {credentials: 'include'})).json();
  const mine = (list.tokens || []).filter(x => x.name === name);
  for (const o of mine) await fetch('/api/v1/tokens/' + o.token_id, {method: 'DELETE', credentials: 'include', headers: h});
  return mine.length;
})"""


async def run(a: argparse.Namespace) -> int:
    password = os.environ.get('OGUN_HUB_PASSWORD')
    if not password:
        print('set OGUN_HUB_PASSWORD (the sign-in form of the identity provider)')
        return 2
    results: list[tuple[bool, str]] = []

    def check(ok: bool, name: str, detail: str = '') -> None:
        results.append((bool(ok), name))
        print(('  ok   ' if ok else '  FAIL ') + name + (f'  [{detail}]' if detail and not ok else ''))

    profile = tempfile.mkdtemp(prefix='ogun-hub-')
    proc = subprocess.Popen(
        [
            a.chrome or find_chrome(),
            '--headless=new',
            f'--remote-debugging-port={a.port}',
            f'--user-data-dir={profile}',
            '--window-size=1440,900',
            '--hide-scrollbars',
            '--disable-gpu',
            '--no-first-run',
            'about:blank',
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    try:
        ws_url = None
        for _ in range(60):
            try:
                page = next((t for t in requests.get(f'http://127.0.0.1:{a.port}/json', timeout=1).json() if t.get('type') == 'page'), None)
                if page:
                    ws_url = page['webSocketDebuggerUrl']
                    break
            except Exception:
                pass
            time.sleep(0.25)
        if not ws_url:
            print('Chrome did not expose a page target')
            return 2
        async with websockets.connect(ws_url, max_size=64 * 1024 * 1024) as ws:
            c = Cdp(ws)
            asyncio.create_task(c.pump())
            for m in ('Page.enable', 'Runtime.enable', 'Network.enable', 'Log.enable'):
                await c.call(m)
            await c.call('Emulation.setDeviceMetricsOverride', width=1440, height=900, deviceScaleFactor=1, mobile=False)
            errors: list[str] = []
            failed: list[str] = []

            async def watch():
                while True:
                    e = await c.events.get()
                    p = e['params']
                    if e['method'] == 'Runtime.exceptionThrown':
                        d = p['exceptionDetails']
                        errors.append('exception: ' + (d.get('exception', {}).get('description') or d.get('text', ''))[:200])
                    elif e['method'] == 'Log.entryAdded' and p['entry'].get('level') in ('error', 'warning'):
                        errors.append(f"log {p['entry'].get('level')}: {p['entry'].get('text', '')[:160]}")
                    elif e['method'] == 'Network.responseReceived' and p['response']['status'] >= 400:
                        failed.append(f"{p['response']['status']} {p['response']['url'].split('?')[0][-80:]}")

            asyncio.create_task(watch())

            async def wait(cond: str, timeout: float = 15.0) -> bool:
                end = time.time() + timeout
                while time.time() < end:
                    try:
                        if await c.eval(f'!!({cond})'):
                            return True
                    except Exception:
                        pass
                    await asyncio.sleep(0.2)
                return False

            async def shot(name: str) -> None:
                r = await c.call('Page.captureScreenshot', format='png')
                (out / f'{name}.png').write_bytes(base64.b64decode(r['data']))

            print('sign-in')
            if a.enroll_file:
                await c.call('Page.navigate', url=Path(a.enroll_file).read_text().strip().splitlines()[0])
            else:
                await c.call('Page.navigate', url=a.base.rstrip('/') + '/auth/login?provider=demo')
            await asyncio.sleep(2)
            if await c.eval("location.pathname.startsWith('/enroll')"):
                await c.eval("(() => { const l = [...document.querySelectorAll('a,button')].find(e => /sign in/i.test(e.textContent)); if (l) l.click() })()")
                await asyncio.sleep(2.5)
            have_form = await wait("document.querySelector('input[name=login]')", 15)
            check(have_form, 'the identity provider shows its sign-in form')
            if have_form:
                await c.eval("document.querySelector('input[name=login]').value = " + json.dumps(a.user))
                await c.eval(
                    "(() => { document.querySelector('input[name=password]').value = "
                    + json.dumps(password)
                    + "; document.querySelector('#submit-login, button[type=submit]').click() })()"
                )
            ok = await wait("location.pathname === '/' && document.querySelector('#app')", 20)
            check(ok, 'the callback lands on Studio-web')
            await asyncio.sleep(2)

            me = await c.eval(ME_JS)
            check(bool(me) and me['status'] == 200 and me['kind'] == 'session', 'GET /api/v1/me answers a session', json.dumps(me))
            check(bool(me) and len(me['roles']) > 0 and me['csrf'], 'the session has a role and a CSRF token')
            st = await c.eval(STATUS_JS)
            check(bool(st) and st['status'] == 200 and st['profile'] == 'server', 'GET /api/v1/status reaches the real daemon', json.dumps(st))
            if st:
                print(f"       daemon {st['version']}, {st['workflows']} workflow(s)")
            role = me['roles'][0] if me and me['roles'] else '?'
            label = f"{me['user']} · {role}" if me and me.get('user') else ''
            check(bool(label) and await wait(f'document.body.innerText.includes({json.dumps(label)})', 8), 'Studio shows who is signed in and their role', label)

            if a.make_token:
                t = await c.eval(TOKEN_JS)
                check(bool(t) and t['status'] == 200 and str(t.get('secret') or '').startswith('ogt1_'), 'a viewer API token can be created in a session')
                if t and t.get('secret'):
                    Path(a.make_token).write_text(t['secret'])
                    print(f'       token written to {a.make_token} (not shown; revoke it when done)')

            if a.revoke_token:
                n = await c.eval(REVOKE_JS + '(' + json.dumps(a.revoke_token) + ')')
                print(f'       revoked {n} token(s) named {a.revoke_token}')

            print('pages')
            for route in a.routes or ROUTES:
                await c.eval(f'location.hash = {json.dumps(route)}; true')
                # The connector catalogue is a 175 kB chunk and the hub may be behind a slow tunnel.
                await wait("document.querySelector('main h1')", 10)
                await asyncio.sleep(0.8)
                h1 = await c.eval("document.querySelector('main h1')?.textContent ?? ''")
                missing = await c.eval("document.body.innerText.includes('No such page')")
                check((bool(h1) or route == '#/') and not missing, f'{route} renders a page', h1 or 'no heading')
                await shot('studio' + route.replace('#', '').replace('/', '-'))
            check(not errors, 'no console errors or exceptions', '; '.join(errors[:3]))
            check(not failed, 'no failed requests', '; '.join(sorted(set(failed))[:4]))
    finally:
        proc.kill()
        shutil.rmtree(profile, ignore_errors=True)
    bad = [n for ok, n in results if not ok]
    print(f'\n{len(results) - len(bad)}/{len(results)} checks passed')
    return 1 if bad else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--base', default='http://localhost:18443')
    ap.add_argument('--user', default='demo@ogun.local')
    ap.add_argument('--enroll-file', help='a file whose first line is the owner enrolment link (consumed once)')
    ap.add_argument('--make-token', help='write a new one-day viewer API token to this file')
    ap.add_argument('--revoke-token', metavar='NAME', help='delete the API tokens with this name (for example desktop-check)')
    ap.add_argument('--out', default='.design-reference/hub')
    ap.add_argument('--routes', nargs='*')
    ap.add_argument('--chrome')
    ap.add_argument('--port', type=int, default=9555)
    return asyncio.run(run(ap.parse_args()))


if __name__ == '__main__':
    sys.exit(main())
