"""The agent screen's flows for smoke_studio.py: what a person does in a conversation and what the boundary allows.

Not a script of the replies: it checks the rules. A read-only thread cannot store a workflow and says so, the button on
the card can, an Operate thread starts a run itself, a tool switched off in Settings is refused, nothing in a card
decides an approval, a secret-looking instruction cannot be saved, and Stop stops.
"""

from __future__ import annotations

import asyncio
import json


async def type_text(p, text: str) -> None:
    await p.js(
        "(t => { const a = document.querySelector('.composer textarea'); const d = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value'); "
        "d.set.call(a, t); a.setSelectionRange(t.length, t.length); a.dispatchEvent(new Event('input', {bubbles: true})); a.focus() })(" + json.dumps(text) + ')'
    )
    await asyncio.sleep(0.25)


async def send(p, text: str) -> None:
    await type_text(p, text)
    await p.key('enter')


async def idle(p, timeout: float = 45.0) -> bool:
    await asyncio.sleep(0.8)
    return await p.wait("!document.querySelector('.sendbtn.stop')", timeout)


async def thread_text(p) -> str:
    return await p.js("document.querySelector('.timeline')?.innerText ?? ''") or ''


async def new_thread(p) -> None:
    await p.js("document.querySelector('.newthread').click(); true")
    await asyncio.sleep(0.5)


async def set_access(p, label: str) -> None:
    await p.js("document.querySelector('.chipbtn[aria-label^=\"Access\"]').click(); true")
    await asyncio.sleep(0.2)
    await p.click_text('.popmenu button', label)
    await asyncio.sleep(0.3)


async def run_agent_checks(p) -> None:
    print('agent: the first screen')
    await p.wait("document.querySelector('.hero h1')", 8)
    p.check(await p.js("location.hash.startsWith('#/agent')"), 'the app opens on the agent screen when the daemon has one')
    p.check(await p.js("document.querySelector('.hero h1')?.textContent === 'What should Ogun work on?'"), 'the empty thread asks what to work on')
    p.check(await p.js("document.querySelectorAll('.starters button').length === 6"), 'six ready-made starting points')
    p.check(await p.js("document.querySelector('.sendbtn').disabled"), 'send is disabled until something is typed')
    p.check(await p.wait("document.querySelectorAll('.threadrow').length >= 2", 8), 'earlier threads are listed in the sidebar')
    p.check(await p.js("document.querySelector('.composer').textContent.includes('Ogun agent')"), 'the composer says that no model is called')

    print('agent: a question about the daemon')
    await p.click_text('.starters button', 'What is running')
    p.check(await p.wait("location.hash.startsWith('#/agent/thr_')", 6), 'sending starts a thread and opens it')
    p.check(await idle(p), 'the agent finishes its turn')
    text = await thread_text(p)
    p.check('What is running' in text and 'tool call' in text, 'the question, then a folded line of tool calls')
    p.check('waiting for a person' in text and 'trips-backfill' in text, 'the answer is read from the daemon')
    p.check(await p.js("!!document.querySelector('.acard')"), 'pending approvals come as a card')
    p.check(
        await p.js("[...document.querySelectorAll('.acard button')].every(b => !/approve|deny|decide/i.test(b.textContent))"),
        'no button in a card decides an approval',
    )
    await p.js("document.querySelector('.grouphead').click(); true")
    await asyncio.sleep(0.3)
    p.check(await p.js("document.querySelectorAll('.tool').length >= 3"), 'the tool calls open to show each one')
    await p.js("document.querySelector('.toolhead').click(); true")
    await asyncio.sleep(0.2)
    p.check(await p.js("document.querySelector('.toolbody')?.textContent.includes('The daemon answered')"), 'and what the daemon answered')

    print('agent: read only stores nothing, a person can')
    await new_thread(p)
    await send(p, 'Draft a daily workflow that lists trips per station from the open-trips archive')
    p.check(await idle(p), 'a draft finishes')
    text = await thread_text(p)
    p.check('cannot store it' in text, 'a read-only thread says it cannot store the workflow')
    p.check(await p.js("document.querySelector('.acard .chip.ok')?.textContent === 'valid'"), 'the proposal was validated before it was shown')
    p.check(await p.js("document.querySelector('.acard .chip:not(.ok):not(.warn)')?.textContent === 'not stored'"), 'and is marked not stored')
    p.check(await p.click_text('.acard footer .btn', 'Store as you'), 'the person presses Store')
    p.check(await p.wait("[...document.querySelectorAll('.acard .chip')].some(c => /^stored as v1$/.test(c.textContent))", 6), 'the card then says stored as v1')
    p.check('Stored as' in await thread_text(p) and 'by you' in await thread_text(p), 'and the thread says who stored it')
    p.check(await p.wait("document.querySelector('.nav')?.textContent.includes('trips-station')", 6), 'the workflow is in the daemon')

    print('agent: Stop')
    await new_thread(p)
    await send(p, 'Backfill trips-backfill since 2019')
    await asyncio.sleep(0.5)
    p.check(await p.js("!!document.querySelector('.sendbtn.stop')"), 'while it works the send button becomes Stop')
    await p.js("document.querySelector('.sendbtn.stop').click(); true")
    p.check(await idle(p, 10), 'Stop ends the turn')
    p.check('Stopped' in await thread_text(p), 'and the thread says it stopped')

    print('agent: access levels')
    await new_thread(p)
    await set_access(p, 'Operate')
    await send(p, 'Backfill trips-backfill since 2019')
    p.check(await idle(p, 60), 'an Operate thread finishes a backfill request')
    text = await thread_text(p)
    p.check('I started it' in text, 'with Operate access the agent starts the run itself')
    p.check(await p.js("document.body.textContent.includes('Run of trips-backfill')"), 'and a run card follows it')
    p.check('Operate' in (await p.js("document.querySelector('.chipbtn[aria-label^=\"Access\"]').textContent") or ''), 'the composer shows the access of the thread')

    await new_thread(p)
    await set_access(p, 'Author')
    await send(p, 'Draft a weekly workflow that lists trips per station from the open-trips archive')
    p.check(await idle(p, 60), 'an Author thread finishes a draft')
    p.check('Stored as' in await thread_text(p) and 'It has not run' in await thread_text(p), 'with Author access the agent stores it but does not run it')

    print('agent: what Settings switches off')
    await p.goto('#/settings/agent', 1.2)
    p.check(await p.js("document.querySelector('main h1')?.textContent === 'Agent'"), 'Settings has an Agent page')
    await p.js("document.querySelector('button[role=switch][aria-label=\"Allow backfills.plan\"]').click(); true")
    await asyncio.sleep(0.6)
    await p.goto('#/agent', 0.8)
    await send(p, 'Backfill trips-backfill since 2019')
    p.check(await idle(p, 40), 'a thread with a tool switched off still finishes')
    p.check('1 refused' in (await p.js("document.querySelector('.grouphead')?.textContent") or ''), 'the tool that is switched off is refused, and the folded line says so')
    await p.js("document.querySelector('.grouphead').click(); true")
    await asyncio.sleep(0.3)
    p.check('switched off in Settings' in (await p.js("document.querySelector('.tool.denied')?.textContent") or ''), 'and a row says why')
    await p.goto('#/settings/agent', 1.0)
    await p.js("document.querySelector('button[role=switch][aria-label=\"Allow backfills.plan\"]').click(); true")
    await asyncio.sleep(0.5)

    print('agent: instructions')
    await p.js(
        "(() => { const a = document.querySelector('textarea.instructions'); const d = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value'); "
        "d.set.call(a, 'Use this key: api_key = sk-abcdef0123456789abcdef'); a.dispatchEvent(new Event('input', {bubbles: true})) })()"
    )
    await asyncio.sleep(0.3)
    p.check(await p.js("document.body.textContent.includes('looks like a secret')"), 'instructions that look like a secret are flagged')
    p.check(await p.js("[...document.querySelectorAll('button')].find(b => b.textContent.includes('Save instructions')).disabled"), 'and cannot be saved')

    print('agent: mentions, commands and the side panel')
    await p.goto('#/agent', 0.8)
    await new_thread(p)
    await type_text(p, 'Explain @trips')
    p.check(await p.wait("document.querySelector('.mentionmenu')", 4), '@ opens a menu of what can be pointed at')
    await p.key('enter')
    p.check(await p.js("document.querySelector('.composer textarea').value.includes('@trips-backfill')"), 'choosing inserts the name')
    p.check(await p.js("!!document.querySelector('.cmp-mentions .chip')"), 'and attaches it as context')
    await p.key('enter')
    p.check(await idle(p, 30), 'a question about a workflow finishes')
    p.check('Operator' in await thread_text(p) and 'trips-backfill' in await thread_text(p), 'it explains the workflow that was pointed at')
    await new_thread(p)
    await type_text(p, '/')
    p.check(await p.js("document.querySelectorAll('.mentionmenu button').length >= 5"), '/ lists ready-made requests')
    await p.key('escape')
    p.check(await p.js("!document.querySelector('.mentionmenu')"), 'and Escape closes the list')

    await p.click_text('.threadrow', 'Backfill')
    await asyncio.sleep(0.6)
    await p.js("document.querySelector('[aria-label=\"Thread details\"]').click(); true")
    p.check(await p.wait("document.querySelector('.agent-side')", 3), 'the thread details open beside the conversation')
    p.check(await p.js("document.querySelector('.agent-side').textContent.includes('Never, at any access')"), 'with what no setting changes')
    p.check(await p.js("[...document.querySelectorAll('.toollist li')].some(li => li.classList.contains('off'))"), 'and the tools this access has closed')
    p.check(await p.js("![...document.querySelectorAll('.toollist li')].some(li => /decide|approve|secret|shell/i.test(li.textContent))"), 'no tool decides, reads a secret or runs a shell')
    await p.click_text('.dactions .btn', 'Rename')
    await asyncio.sleep(0.4)
    await p.js(
        "(() => { const i = document.querySelector('.renameform input'); const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value'); d.set.call(i, 'Backfill check'); i.dispatchEvent(new Event('input', {bubbles: true})) })()"
    )
    await asyncio.sleep(0.3)
    await p.js("document.querySelector('.renameform button').click(); true")
    p.check(await p.wait("[...document.querySelectorAll('.threadrow .name')].some(n => n.textContent === 'Backfill check')", 4), 'a thread can be renamed')
    await p.click_text('.dactions .btn', 'Delete')
    p.check(await p.wait("location.hash === '#/agent'", 4), 'and deleted')
    p.check(await p.js("![...document.querySelectorAll('.threadrow .name')].some(n => n.textContent === 'Backfill check')"), 'it is gone from the list')

    print('agent: keyboard')
    await p.goto('#/runs', 0.6)
    await p.key('ctrl+shift+o')
    p.check(await p.wait("location.hash === '#/agent'", 3), 'Ctrl+Shift+O starts a new thread from anywhere')
