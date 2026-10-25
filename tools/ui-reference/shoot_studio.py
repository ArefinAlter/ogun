#!/usr/bin/env python3
"""Screenshot Ogun Studio pages with headless Chrome over the DevTools protocol.

    python shoot_studio.py --base http://localhost:4174 --out DIR            # the built-in page list
    python shoot_studio.py --base http://localhost:5173 --out DIR --plan plan.json

A plan is {"shots": [{"name": "runs", "hash": "#/runs"}, {"name": "run", "hash": "#/runs",
"then": "document.querySelector('table a').getAttribute('href')"}, {"name": "dark", "hash": "#/",
"theme": "dark"}, {"name": "palette", "hash": "#/", "key": "k"}]}: `then` is JavaScript whose string
result is navigated to next; `theme` sets data-theme on <html>; `click` clicks a selector; `js` runs
JavaScript for its side effects (a click on a found element); `skip` takes no picture; `key` presses Ctrl+<key>; `width`/`height` change the viewport (default 1440x900); `settle_ms` waits
(default 1400). Needs Chrome, and the Python packages `websockets` and `requests`.
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

CHROME = [
    r'C:\Program Files\Google\Chrome\Application\chrome.exe',
    r'C:\Program Files (x86)\Google\Chrome\Application\chrome.exe',
    r'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
]

DEFAULT_PLAN = {
    'shots': [
        {'name': '01-overview', 'hash': '#/'},
        {'name': '02-workflows', 'hash': '#/workflows'},
        {'name': '03-workflow-rebalance', 'hash': '#/workflows/rebalance-plan'},
        {'name': '04-runs', 'hash': '#/runs'},
        {'name': '05-run-backfill', 'hash': '#/runs', 'then': "[...document.querySelectorAll('table tbody tr')].find(r => r.textContent.includes('trips-backfill')).querySelector('a').getAttribute('href')", 'full_page': False},
        {'name': '05b-run-backfill-graph', 'js': "[...document.querySelectorAll('.tabs button')].find(b => b.textContent.trim() === 'Graph').click()", 'settle_ms': 800},
        {'name': '05c-run-backfill-events', 'js': "[...document.querySelectorAll('.tabs button')].find(b => b.textContent.trim().startsWith('Events')).click()", 'settle_ms': 4500},
        {'name': '06-partitions-trips', 'hash': '#/partitions?workflow=trips-backfill'},
        {'name': '07-approvals', 'hash': '#/approvals'},
        {'name': '08-approval-detail', 'hash': '#/approvals', 'then': "document.querySelector('table tbody a').getAttribute('href')"},
        {'name': '08b-approval-confirm-armed', 'js': "document.querySelector('.decide-row .btn').click()", 'settle_ms': 2800},
        {'name': '08c-approval-confirm-close', 'js': "document.querySelector('.dialog .decide-row .btn').click()", 'settle_ms': 500, 'skip': True},
        {'name': '09-run-rebalance', 'hash': '#/runs?workflow=rebalance-plan', 'then': "document.querySelector('table tbody a').getAttribute('href')"},
        {'name': '09b-artifacts', 'js': "document.querySelector('a.btn[href*=\"artifacts\"]').click()", 'settle_ms': 1600},
        {'name': '09c-artifact-csv-preview', 'js': "[...document.querySelectorAll('table tbody tr')].find(r => r.textContent.includes('text/csv')).querySelector('a').click()", 'settle_ms': 1600},
        {'name': '09d-artifact-lineage', 'js': "[...document.querySelectorAll('.tabs button')].filter(b => b.textContent.trim() === 'Lineage').pop().click()", 'settle_ms': 1200},
        {'name': '10-streams', 'hash': '#/streams'},
        {'name': '11-stream-detail', 'hash': '#/streams/dock-telemetry/docks'},
        {'name': '12-edges', 'hash': '#/edges'},
        {'name': '13-settings', 'hash': '#/settings'},
        {'name': '14-about', 'hash': '#/about'},
        {'name': '15-palette', 'hash': '#/', 'key': 'k'},
        {'name': '16-agent', 'hash': '#/agent'},
        {'name': '17-overview-dark', 'hash': '#/', 'theme': 'dark'},
        {'name': '18-approval-dark', 'hash': '#/approvals', 'then': "document.querySelector('table tbody a').getAttribute('href')", 'theme': 'dark'},
        {'name': '19-run-backfill-dark', 'hash': '#/runs', 'then': "[...document.querySelectorAll('table tbody tr')].find(r => r.textContent.includes('trips-backfill')).querySelector('a').getAttribute('href')", 'theme': 'dark', 'full_page': False},
        {'name': '20-overview-phone', 'hash': '#/', 'width': 390, 'height': 844},
    ]
}


def find_chrome() -> str:
    for c in CHROME:
        if os.path.exists(c):
            return c
    found = shutil.which('chrome') or shutil.which('google-chrome') or shutil.which('chromium')
    if found:
        return found
    raise SystemExit('no Chrome found; set --chrome')


class Cdp:
    def __init__(self, ws):
        self.ws = ws
        self.n = 0
        self.pending: dict[int, asyncio.Future] = {}
        self.events: asyncio.Queue = asyncio.Queue()

    async def pump(self):
        async for raw in self.ws:
            m = json.loads(raw)
            if 'id' in m and m['id'] in self.pending:
                self.pending.pop(m['id']).set_result(m)
            elif 'method' in m:
                await self.events.put(m)

    async def call(self, method: str, **params):
        self.n += 1
        fut = asyncio.get_event_loop().create_future()
        self.pending[self.n] = fut
        await self.ws.send(json.dumps({'id': self.n, 'method': method, 'params': params}))
        m = await fut
        if 'error' in m:
            raise RuntimeError(f'{method}: {m["error"]}')
        return m.get('result', {})

    async def eval(self, js: str):
        r = await self.call('Runtime.evaluate', expression=js, returnByValue=True, awaitPromise=True)
        return r.get('result', {}).get('value')


async def run(args) -> int:
    plan = DEFAULT_PLAN
    if args.plan:
        plan = json.loads(Path(args.plan).read_text(encoding='utf-8'))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    profile = tempfile.mkdtemp(prefix='ogun-shoot-')
    port = args.port
    proc = subprocess.Popen(
        [
            args.chrome or find_chrome(),
            '--headless=new',
            f'--remote-debugging-port={port}',
            f'--user-data-dir={profile}',
            '--window-size=1440,900',
            '--hide-scrollbars',
            '--disable-gpu',
            '--no-first-run',
            '--no-default-browser-check',
            'about:blank',
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        ws_url = None
        for _ in range(60):
            try:
                targets = requests.get(f'http://127.0.0.1:{port}/json', timeout=1).json()
                page = next((t for t in targets if t.get('type') == 'page'), None)
                if page:
                    ws_url = page['webSocketDebuggerUrl']
                    break
            except Exception:
                pass
            time.sleep(0.25)
        if not ws_url:
            raise SystemExit('Chrome did not expose a page target')
        async with websockets.connect(ws_url, max_size=64 * 1024 * 1024) as ws:
            cdp = Cdp(ws)
            pump = asyncio.create_task(cdp.pump())
            await cdp.call('Page.enable')
            await cdp.call('Runtime.enable')
            width, height = 1440, 900
            await cdp.call('Emulation.setDeviceMetricsOverride', width=width, height=height, deviceScaleFactor=1, mobile=False)
            await cdp.call('Page.navigate', url=args.base + '/')
            await asyncio.sleep(1.5)
            manifest = []
            for shot in plan['shots']:
                w, h = shot.get('width', 1440), shot.get('height', 900)
                if (w, h) != (width, height):
                    width, height = w, h
                    await cdp.call('Emulation.setDeviceMetricsOverride', width=w, height=h, deviceScaleFactor=1, mobile=w < 760)
                theme = shot.get('theme')
                await cdp.eval(
                    "(() => { const r = document.documentElement; "
                    + (f"r.dataset.scheme = {json.dumps(theme)};" if theme else "delete r.dataset.scheme;")
                    + " return true })()"
                )
                if 'hash' in shot:
                    await cdp.eval(f'(() => {{ location.hash = {json.dumps(shot["hash"].lstrip("#"))}; return true }})()')
                    await asyncio.sleep(shot.get('settle_ms', 1400) / 1000)
                if 'then' in shot:
                    target = await cdp.eval(shot['then'])
                    if isinstance(target, str) and target:
                        await cdp.eval(f'(() => {{ location.hash = {json.dumps(target.lstrip("#"))}; return true }})()')
                        await asyncio.sleep(shot.get('settle_ms', 1400) / 1000)
                    else:
                        print(f'{shot["name"]}: `then` returned nothing; capturing the list page instead')
                if 'js' in shot:
                    await cdp.eval(f'(() => {{ try {{ {shot["js"]} }} catch (e) {{ console.warn(e) }} return true }})()')
                    await asyncio.sleep(shot.get('settle_ms', 1400) / 1000)
                if shot.get('skip'):
                    continue
                if 'click' in shot:
                    await cdp.eval(f'(() => {{ const el = document.querySelector({json.dumps(shot["click"])}); if (el) el.click(); return !!el }})()')
                    await asyncio.sleep(shot.get('settle_ms', 1400) / 1000)
                if 'key' in shot:
                    parts = shot['key'].lower().split('+')
                    k = parts[-1]
                    mods = (1 if 'alt' in parts else 0) | (2 if 'ctrl' in parts else 0) | (8 if 'shift' in parts else 0)
                    if len(parts) == 1:
                        mods = 2  # a bare letter means Ctrl+letter, as in the first plans
                    code = f'Key{k.upper()}' if len(k) == 1 and k.isalpha() else (f'Digit{k}' if k.isdigit() else k.title())
                    for kind in ('keyDown', 'keyUp'):
                        await cdp.call('Input.dispatchKeyEvent', type=kind, key=k, code=code, modifiers=mods, windowsVirtualKeyCode=ord(k.upper()) if len(k) == 1 else 0)
                    await asyncio.sleep(0.8)
                metrics = await cdp.call('Page.getLayoutMetrics')
                content = metrics.get('cssContentSize') or metrics.get('contentSize')
                full = shot.get('full_page', True)
                clip = {'x': 0, 'y': 0, 'width': width, 'height': max(height, int(content['height'])) if full else height, 'scale': 1}
                if full and clip['height'] > height:
                    await cdp.call('Emulation.setDeviceMetricsOverride', width=width, height=clip['height'], deviceScaleFactor=1, mobile=width < 760)
                    await asyncio.sleep(0.3)
                shot_r = await cdp.call('Page.captureScreenshot', format='png', clip=clip, captureBeyondViewport=True)
                if full and clip['height'] > height:
                    await cdp.call('Emulation.setDeviceMetricsOverride', width=width, height=height, deviceScaleFactor=1, mobile=width < 760)
                path = out / f'{shot["name"]}.png'
                path.write_bytes(base64.b64decode(shot_r['data']))
                print(f'{path}  {width}x{clip["height"]}')
                manifest.append({'name': shot['name'], 'file': path.name, 'hash': await cdp.eval('location.hash'), 'size': [width, clip['height']]})
                if 'key' in shot:
                    await cdp.call('Input.dispatchKeyEvent', type='keyDown', key='Escape', code='Escape', windowsVirtualKeyCode=27)
                    await cdp.call('Input.dispatchKeyEvent', type='keyUp', key='Escape', code='Escape', windowsVirtualKeyCode=27)
            (out / 'manifest.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
            pump.cancel()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
        shutil.rmtree(profile, ignore_errors=True)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--base', default='http://localhost:4174', help='where Studio is served')
    ap.add_argument('--out', default='.design-reference/studio')
    ap.add_argument('--plan')
    ap.add_argument('--chrome')
    ap.add_argument('--port', type=int, default=9333)
    args = ap.parse_args()
    return asyncio.run(run(args))


if __name__ == '__main__':
    sys.exit(main())
