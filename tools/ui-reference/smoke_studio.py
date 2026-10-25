#!/usr/bin/env python3
"""Drives Ogun Studio in headless Chrome and checks that its flows work, not just that its pages render.

    python smoke_studio.py --base http://localhost:4174            # the scripted build

It reports every console error and uncaught exception the page raised, and exits 1 on any failed check. Needs
Chrome, `websockets` and `requests`.
"""

from __future__ import annotations

import argparse
import asyncio
import json
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
from smoke_agent import run_agent_checks  # noqa: E402

SECTIONS = {
    'general': 'General',
    'appearance': 'Appearance',
    'keybindings': 'Keybindings',
    'evidence': 'Evidence',
    'providers': 'Providers',
    'connectors': 'Connectors',
    'connections': 'Connections',
    'schedules': 'Schedules',
    'sources': 'Workflow sources',
    'storage': 'Storage',
    'environments': 'Environments',
    'security': 'Security',
    'archive': 'Archive',
}


class Page:
    def __init__(self, cdp: Cdp):
        self.cdp = cdp
        self.errors: list[str] = []
        self.results: list[tuple[bool, str]] = []

    async def js(self, expr: str):
        return await self.cdp.eval(expr)

    async def goto(self, hash_: str, settle: float = 0.9):
        await self.js(f'location.hash = {json.dumps(hash_.lstrip("#"))}; true')
        await asyncio.sleep(settle)

    async def wait(self, cond: str, timeout: float = 6.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            try:
                if await self.js(f'!!({cond})'):
                    return True
            except Exception:
                pass
            await asyncio.sleep(0.15)
        return False

    async def click_text(self, selector: str, text: str) -> bool:
        return bool(await self.js(f"""(() => {{ const el = [...document.querySelectorAll({json.dumps(selector)})].find(e => e.textContent.trim().includes({json.dumps(text)}) && !e.disabled); if (el) {{ el.click(); return true }} return false }})()"""))

    async def set_input(self, selector: str, value: str, event: str = 'input') -> bool:
        return bool(
            await self.js(
                f"""(() => {{ const el = document.querySelector({json.dumps(selector)}); if (!el) return false; const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype; Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, {json.dumps(value)}); el.dispatchEvent(new Event({json.dumps(event)}, {{ bubbles: true }})); return true }})()"""
            )
        )

    async def key(self, combo: str):
        parts = combo.lower().split('+')
        k = parts[-1]
        mods = (1 if 'alt' in parts else 0) | (2 if 'ctrl' in parts else 0) | (8 if 'shift' in parts else 0)
        code = f'Key{k.upper()}' if len(k) == 1 and k.isalpha() else (f'Digit{k}' if k.isdigit() else k.title())
        named = {'escape': ('Escape', 27), 'enter': ('Enter', 13), 'backspace': ('Backspace', 8)}
        key_name, vk = named.get(k, (k, ord(k.upper()) if len(k) == 1 else 0))
        for kind in ('keyDown', 'keyUp'):
            await self.cdp.call('Input.dispatchKeyEvent', type=kind, key=key_name, code=code, modifiers=mods, windowsVirtualKeyCode=vk)
        await asyncio.sleep(0.4)

    def check(self, ok: bool, name: str, detail: str = ''):
        self.results.append((bool(ok), name))
        print(('  ok   ' if ok else '  FAIL ') + name + (f'  [{detail}]' if detail and not ok else ''))


async def run_checks(p: Page, expect_no_storage: bool = False, expect_frozen: bool = False) -> None:
    if expect_frozen:
        print('frozen prototype (as in the Tauri shell)')
        p.check(await p.js('Object.isFrozen(Object.prototype)'), 'Object.prototype is frozen before the app runs')
    # ---- the agent screen: the first page when the daemon has one
    await run_agent_checks(p)

    # ---- every settings page loads and says what it is
    print('settings pages')
    for sid, label in SECTIONS.items():
        await p.goto(f'#/settings/{sid}', 1.2)
        h1 = await p.js("document.querySelector('main h1')?.textContent ?? ''")
        p.check(h1 == label, f'/settings/{sid} shows "{label}"', h1)

    # ---- appearance: a theme and a scheme really change the document
    print('appearance')
    await p.goto('#/settings/appearance')
    await p.click_text('.theme-pick', 'Grove')
    await asyncio.sleep(0.3)
    p.check(await p.js("document.documentElement.dataset.themeId === 'grove'"), 'choosing Grove sets the theme')
    p.check(await p.js("getComputedStyle(document.documentElement).getPropertyValue('--primary').includes('oklch')"), 'Grove sets its own primary colour')
    await p.click_text('.scheme-card', 'Dark')
    await asyncio.sleep(0.3)
    p.check(await p.js("document.documentElement.dataset.scheme === 'dark'"), 'choosing Dark sets the scheme')
    await p.click_text('.theme-pick', 'Ogun')
    await p.click_text('.scheme-card', 'Light')
    await p.js("(() => { const r = document.querySelector('input[aria-label=\"Contrast\"]'); r.value = 60; r.dispatchEvent(new Event('input', {bubbles: true})); return true })()")
    await asyncio.sleep(0.2)
    p.check(await p.js("getComputedStyle(document.documentElement).getPropertyValue('--c-low').trim() === '0.6'"), 'the contrast slider changes the rules')
    await p.js("(() => { const r = document.querySelector('input[aria-label=\"Contrast\"]'); r.value = 100; r.dispatchEvent(new Event('input', {bubbles: true})); return true })()")
    await p.click_text('.btn', 'Create theme')
    await asyncio.sleep(0.4)
    await p.set_input('.dialog input[maxlength]', 'Harbour')
    await p.click_text('.dialog .btn.primary', 'Save theme')
    await asyncio.sleep(0.4)
    p.check(await p.js("[...document.querySelectorAll('.theme-name')].some(e => e.textContent === 'Harbour')"), 'a custom theme can be created')
    p.check(await p.js("document.documentElement.dataset.themeId === 'harbour'"), 'and is applied at once')
    await p.js("document.querySelector('.theme-card.on .icon-btn[aria-label^=\"Delete\"]').click(); true")
    await p.click_text('.theme-pick', 'Ogun')

    # ---- the command palette and the theme picker
    print('palette and shortcuts')
    await p.goto('#/')
    await p.key('ctrl+k')
    p.check(await p.wait("document.querySelector('.palette')"), 'Ctrl+K opens the palette')
    await p.set_input('.palette input', 'datadog')
    await asyncio.sleep(0.5)
    p.check(await p.js("[...document.querySelectorAll('.palette .item .name')].some(e => e.textContent.includes('Datadog'))"), 'the palette finds a connector')
    await p.key('escape')
    await p.key('ctrl+alt+a')
    p.check(await p.wait("document.querySelector('.palette .group')?.textContent === 'Change theme'"), 'Ctrl+Alt+A opens the theme picker')
    await p.key('escape')
    await p.key('alt+3')
    p.check(await p.wait("location.hash.startsWith('#/runs')"), 'Alt+3 goes to Runs')

    # ---- keybindings: rebinding takes effect
    print('keybindings')
    await p.goto('#/settings/keybindings')
    await p.js("document.querySelector('button[aria-label^=\"Shortcut for Show or hide the sidebar\"]').click(); true")
    await asyncio.sleep(0.3)
    await p.key('ctrl+shift+y')
    await asyncio.sleep(0.3)
    p.check(await p.js("document.querySelector('button[aria-label^=\"Shortcut for Show or hide the sidebar\"]').textContent.includes('Y')"), 'a shortcut can be re-recorded')
    await p.key('ctrl+shift+y')
    p.check(await p.js("document.querySelector('.shell').classList.contains('side-closed')"), 'the new shortcut hides the sidebar')
    await p.key('ctrl+shift+y')
    await p.click_text('.btn', 'Reset all')

    # ---- connectors
    print('connectors')
    await p.goto('#/settings/connectors', 1.6)
    n_all = await p.js("document.querySelectorAll('.ccard').length")
    p.check(n_all and n_all >= 90, 'the catalog lists every connector', str(n_all))
    await p.set_input('input[aria-label="Search connectors"]', 'dingtalk')
    await asyncio.sleep(0.4)
    p.check(await p.js("document.querySelectorAll('.ccard').length === 1 && document.querySelector('.ccard .name').textContent.includes('DingTalk')"), 'search narrows to DingTalk')
    await p.set_input('input[aria-label="Search connectors"]', '')
    await p.click_text('.segmented button', 'China')
    await asyncio.sleep(0.4)
    cn = await p.js("[...document.querySelectorAll('.ccard .name')].map(e => e.textContent)")
    p.check(any('Alibaba' in x for x in cn) and any('Tencent' in x for x in cn) and any('Huawei' in x for x in cn), 'the China filter lists Alibaba, Tencent and Huawei', str(len(cn)))
    await p.click_text('.segmented button', 'All')
    await p.goto('#/settings/connectors/datadog', 1.2)
    p.check(await p.js("document.querySelector('main h1').textContent === 'Datadog'"), 'a connector page opens')
    p.check(await p.js("document.body.textContent.includes('api.datadoghq.com')"), 'it lists the host, with the default site filled in')

    # ---- connections: create, test, fail on a missing secret, fix it, pass
    print('connections')
    await p.goto('#/settings/connections/new?connector=bigquery', 1.4)
    await p.set_input('input[aria-label="Connection name"]', 'smoke-bq')
    await p.set_input('input[aria-label="Project"]', 'smoke-project')
    await p.js("document.querySelector('input[aria-label$=\": secret name\"]').focus(); true")
    await asyncio.sleep(0.3)
    sn = await p.js("document.querySelector('input[aria-label$=\": secret name\"]').value")
    p.check(sn == 'smoke-bq_service_account_json', 'the secret name is proposed from the connection name', sn)
    await p.click_text('.btn', 'Save and test')
    p.check(await p.wait("document.querySelector('.testpanel.bad')", 6), 'a test fails while the secret is not stored')
    p.check(await p.js("document.querySelector('.testpanel').textContent.includes('ogun secrets set smoke-bq_service_account_json')"), 'and says which command to run')
    p.check(await p.js("!document.body.textContent.includes('smoke-secret-value')"), 'no secret value anywhere (there is no field for one)')
    await p.click_text('.btn', 'Check again')
    await asyncio.sleep(0.5)
    await p.click_text('.btn', 'Save and test')
    p.check(await p.wait("document.querySelector('.testpanel.ok')", 6), 'the test passes once the secret is stored')
    await p.goto('#/settings/connections', 1.4)
    p.check(await p.js("[...document.querySelectorAll('td a.mono')].some(a => a.textContent === 'smoke-bq')"), 'the connection is listed')
    await p.js("[...document.querySelectorAll('tbody tr')].find(r => r.textContent.includes('smoke-bq')).querySelector('.btn.ghost.danger').click(); true")
    await asyncio.sleep(0.4)
    await p.click_text('.dialog .btn.danger', 'Delete connection')
    await asyncio.sleep(0.6)
    p.check(await p.js("![...document.querySelectorAll('td a.mono')].some(a => a.textContent === 'smoke-bq')"), 'and can be deleted')

    # ---- run defaults and per-workflow overrides
    print('run defaults')
    await p.goto('#/settings/general', 1.3)
    await p.set_input('select[aria-label="Workflow"]', 'ridership-forecast', 'change')
    await asyncio.sleep(0.8)
    p.check(await p.js("document.querySelector('input[aria-label=\"retries\"]').value === '5'"), 'a workflow shows its own override (retries 5)')
    p.check(await p.js("!!document.querySelector('.layers.on')"), 'and marks it as an override')
    await p.set_input('select[aria-label="Workflow"]', '', 'change')
    await asyncio.sleep(0.8)
    p.check(await p.js("document.querySelector('input[aria-label=\"retries\"]').value === '3'"), 'the daemon-wide default is 3')

    # ---- schedules
    print('schedules')
    await p.goto('#/settings/schedules', 1.3)
    before = await p.js("document.querySelectorAll('tbody tr').length")
    await p.click_text('.page-head .btn', 'New schedule')
    await asyncio.sleep(0.4)
    await p.set_input('.dialog select', 'rebalance-plan', 'change')
    await p.set_input('.dialog input.mono', '15 6 * * 1')
    p.check(await p.js("document.querySelector('.dialog .notice').textContent.includes('Every Monday at 06:15 UTC')"), 'the cron is read back in words')
    await p.click_text('.dialog .btn.primary', 'Store with this trigger')
    p.check(await p.wait("document.querySelector('.dialog')?.textContent.includes('rebalance-plan v3')", 4), 'a schedule stores a new workflow version')
    await p.click_text('.dialog .btn.primary', 'Done')
    await asyncio.sleep(0.8)
    after = await p.js("document.querySelectorAll('tbody tr').length")
    p.check(after == before + 1, 'and appears in the table', f'{before} -> {after}')

    # ---- storage cleanup
    print('storage')
    await p.goto('#/settings/storage', 1.3)
    first = await p.js("document.querySelector('.rows .row.stack .desc').textContent")
    await p.click_text('.btn', 'Review cleanup')
    await asyncio.sleep(0.6)
    p.check(await p.js("document.querySelector('.dialog')?.textContent.includes('This removes')"), 'a cleanup shows what it would remove first')
    await p.click_text('.dialog .btn.danger', 'Delete now')
    await asyncio.sleep(1.0)
    second = await p.js("document.querySelector('.rows .row.stack .desc').textContent")
    p.check(first != second, 'and removes it once confirmed', f'{first} -> {second}')
    await p.js("[...document.querySelectorAll('.row')].find(r => r.textContent.includes('Keep what lineage still reads')).querySelector('.switch').disabled")
    p.check(await p.js("[...document.querySelectorAll('.row')].find(r => r.textContent.includes('Keep what lineage still reads')).querySelector('.switch').disabled"), 'the lineage rule cannot be turned off')

    # ---- providers: add a Chinese provider, see the validation, get a valid document
    print('providers')
    await p.goto('#/settings/providers', 1.3)
    await p.click_text('.sgroup-actions .btn', 'Add provider')
    await asyncio.sleep(0.3)
    await p.click_text('.preset', 'Qwen on Alibaba Model Studio (China)')
    await asyncio.sleep(0.4)
    doc = await p.js("document.querySelector('.code-block').textContent")
    parsed = json.loads(doc)
    names = [x['name'] for x in parsed['providers']]
    p.check('qwen_cn' in names, 'a preset adds a ladder entry', str(names))
    q = next(x for x in parsed['providers'] if x['name'] == 'qwen_cn')
    p.check(q['base_url'] == 'https://dashscope.aliyuncs.com/compatible-mode/v1' and q.get('enabled') is False, 'disabled, with the documented base URL')
    await p.js("document.querySelector('.pane-item.on .switch').click(); true")
    await asyncio.sleep(0.3)
    p.check(await p.js("[...document.querySelectorAll('.notice.warn')].some(n => n.textContent.includes('names its model'))"), 'enabling an entry with no model is flagged')
    await p.js("document.querySelector('.pane-item.on .switch').click(); true")

    # ---- evidence: a run's bundle really contains the run, its artifacts and lineage
    print('evidence')
    await p.goto('#/runs?workflow=rebalance-plan', 1.2)
    await p.js("document.querySelector('table tbody a').click(); true")
    await asyncio.sleep(1.4)
    await p.js("window.__blobs = []; const o = URL.createObjectURL.bind(URL); URL.createObjectURL = (b) => { window.__blobs.push(b); return o(b) }; true")
    await p.click_text('.page-head .btn', 'Download evidence')
    await p.wait('window.__blobs.length > 0', 5)
    text = await p.js("window.__blobs[0] ? window.__blobs[0].text() : ''")
    try:
        b = json.loads(text)
    except Exception:
        b = {}
    p.check(b.get('evidence_version') == 0 and b.get('run', {}).get('run_id', '').startswith('orun_'), 'the bundle names the run')
    p.check(len(b.get('artifacts', [])) > 0 and all(len(a['blake3']) == 64 for a in b.get('artifacts', [])), 'it lists artifacts with BLAKE3 digests', str(len(b.get('artifacts', []))))
    p.check(len(b.get('lineage', [])) > 0, 'it carries the OpenLineage events')
    p.check(len(b.get('workflow', {}).get('workflow_hash', '')) == 64 and len(b.get('document_sha256', '')) == 64, 'it carries the workflow hash and its own digest')

    # ---- approvals still work, and an agent-visible text stays marked untrusted
    print('approvals')
    await p.goto('#/approvals', 1.2)
    await p.js("document.querySelector('table tbody a').click(); true")
    await asyncio.sleep(0.9)
    p.check(await p.js("!!document.querySelector('.untrusted')"), 'the requester\'s words are marked as not verified')
    await p.click_text('.decide-row .btn', 'Approve')
    await asyncio.sleep(0.5)
    p.check(await p.js("document.querySelector('.dialog .decide-row .btn:last-child').disabled"), 'the confirm button starts disarmed')
    await asyncio.sleep(2.0)
    p.check(await p.js("!document.querySelector('.dialog .decide-row .btn:last-child').disabled"), 'and arms after 1.5 seconds')
    await p.click_text('.dialog .decide-row .btn', 'Approve code change')
    await asyncio.sleep(1.2)
    p.check(await p.js("document.body.textContent.includes('approved')"), 'a decision is recorded')

    if expect_no_storage:
        print('storage')
        p.check(await p.js('localStorage.length === 0 && sessionStorage.length === 0'), 'nothing is kept in local or session storage')
        p.check(await p.js("document.cookie === ''"), 'no cookie is set')
        p.check(await p.js("(async () => { const dbs = indexedDB.databases ? await indexedDB.databases() : []; return dbs.length === 0 })()"), 'no IndexedDB database exists')


async def main_async(args) -> int:
    profile = tempfile.mkdtemp(prefix='ogun-smoke-')
    proc = subprocess.Popen(
        [args.chrome or find_chrome(), '--headless=new', f'--remote-debugging-port={args.port}', f'--user-data-dir={profile}', '--window-size=1440,900', '--hide-scrollbars', '--disable-gpu', '--no-first-run', 'about:blank'],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        ws_url = None
        for _ in range(60):
            try:
                targets = requests.get(f'http://127.0.0.1:{args.port}/json', timeout=1).json()
                page = next((t for t in targets if t.get('type') == 'page'), None)
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
            cdp = Cdp(ws)
            pump = asyncio.create_task(cdp.pump())
            await cdp.call('Page.enable')
            await cdp.call('Runtime.enable')
            await cdp.call('Log.enable')
            await cdp.call('Emulation.setDeviceMetricsOverride', width=1440, height=900, deviceScaleFactor=1, mobile=False)
            page_ = Page(cdp)

            async def watch():
                while True:
                    m = await cdp.events.get()
                    if m['method'] == 'Runtime.exceptionThrown':
                        d = m['params']['exceptionDetails']
                        page_.errors.append('exception: ' + (d.get('exception', {}).get('description') or d.get('text', ''))[:300])
                    elif m['method'] == 'Log.entryAdded' and m['params']['entry'].get('level') in ('error', 'warning') and m['params']['entry'].get('source') in ('security', 'javascript', 'network', 'other'):
                        e = m['params']['entry']
                        page_.errors.append(f"log {e.get('source')}/{e.get('level')}: {e.get('text', '')[:240]}")
                    elif m['method'] == 'Runtime.consoleAPICalled' and m['params']['type'] in ('error',):
                        page_.errors.append('console.error: ' + ' '.join(str(a.get('value', a.get('description', ''))) for a in m['params']['args'])[:300])

            watcher = asyncio.create_task(watch())
            if args.freeze_prototype:
                # The Tauri shell does this (freezePrototype in tauri.conf.json); a library that assigns to an inherited member then throws.
                await cdp.call('Page.addScriptToEvaluateOnNewDocument', source='Object.freeze(Object.prototype)')
            await cdp.call('Page.navigate', url=args.base + '/')
            await asyncio.sleep(1.8)
            await run_checks(page_, args.expect_no_storage, args.freeze_prototype)
            watcher.cancel()
            pump.cancel()
            print('console')
            page_.check(not page_.errors, 'no console errors or exceptions', '; '.join(page_.errors[:3]))
            failed = [n for ok, n in page_.results if not ok]
            print(f'\n{len(page_.results) - len(failed)}/{len(page_.results)} checks passed')
            return 1 if failed else 0
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
        shutil.rmtree(profile, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--base', default='http://localhost:4174')
    ap.add_argument('--chrome')
    ap.add_argument('--port', type=int, default=9444)
    ap.add_argument('--freeze-prototype', action='store_true', help='freeze Object.prototype before the page runs, as the Tauri shell does')
    ap.add_argument('--expect-no-storage', action='store_true', help='also check that the page kept nothing in browser storage')
    return asyncio.run(main_async(ap.parse_args()))


if __name__ == '__main__':
    sys.exit(main())
