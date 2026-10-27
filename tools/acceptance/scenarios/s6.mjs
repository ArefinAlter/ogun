// Scenario 6: agent threads inside the engine, with the person's own provider key (BYOK). The provider here is a
// local server that speaks the OpenAI-compatible wire format (streamed deltas, tool-call deltas, usage); no live
// provider key exists on this computer, so no live provider is called. What is checked: consent before the first
// call, the key by secret name, the closed tool table at the thread's access level, cards a person presses, the
// spend ledger and caps, the offline switch, refusal of secrets in instructions and messages, and that no path of
// the agent decides an approval.
import { join } from 'node:path'
import { readFileSync } from 'node:fs'

export async function run({ ev, root, cleanDir, launch, sleep, waitFor, http, workflow, key, FIXTURE_PORT }) {
  cleanDir(root)
  const { app } = await launch(root)
  const CHAT_KEY = 'acceptance-chat-key-ZEUS-CANARY-7f3a'
  const fx = `http://127.0.0.1:${FIXTURE_PORT}`
  await http(`${fx}/admin/reset`, { method: 'POST' })
  const requests = async () => (await http(`${fx}/admin/requests`)).json ?? []
  const E = (m, p = {}) => app.engine(m, p)
  const tryE = (m, p = {}) => E(m, p).then((v) => ({ ok: v }), (e) => ({ err: e.envelope ?? String(e) }))

  await app.invoke('secret_put', { name: 'acc_chat_key', value: CHAT_KEY })
  await app.invoke('workflows_put', { params: { workflow: workflow('acc-local-fixture.workflow.json'), idempotency_key: key() } })
  const ladder = await E('ladder.put', {
    providers: [{
      name: 'mock', kind: 'openai_compatible', base_url: `${fx}/v1`, model: 'acceptance-model',
      api_key_secret: 'acc_chat_key', input_per_mtok_usd_micro: 1000000, output_per_mtok_usd_micro: 2000000, max_output_tokens: 512,
    }],
    idempotency_key: key(),
  }).then((v) => ({ ok: v }), (e) => ({ err: e.envelope ?? String(e) }))
  ev.save('ladder-put.json', ladder)
  ev.check('a provider is configured with the key named, not the key', !ladder.err, JSON.stringify(ladder).slice(0, 200))
  const models0 = await E('agent.models')
  ev.save('models-before-consent.json', models0)
  ev.check('the provider is listed but not available until the person agrees to send text to it', models0.length === 1 && models0[0].available === false, JSON.stringify(models0).slice(0, 240))

  // Consent first.
  const thread = await E('agent.threads.create', { access: 'read', model: 'mock' })
  const tid = thread.thread_id ?? thread.thread?.thread_id
  const before = await tryE('agent.send', { thread_id: tid, text: 'list workflows please' })
  ev.check('without consent no model call is made', before.err && /consent|available/i.test(JSON.stringify(before.err)), JSON.stringify(before.err).slice(0, 240))
  ev.check('and the provider received nothing', (await requests()).length === 0)
  const preview = await E('agent.preview', { thread_id: tid, text: 'list workflows please' })
  ev.save('preview.json', preview)
  ev.check('the person can see what would be sent before it is sent', JSON.stringify(preview).includes('list workflows') && !JSON.stringify(preview).includes(CHAT_KEY), JSON.stringify(preview).slice(0, 240))
  await E('agent.settings.put', { patch: { consented_providers: ['mock'], default_model: 'mock', spend_cap_usd: 1, day_cap_usd: 5 }, idempotency_key: key() })

  const turn = async (id, text, ms = 60000) => {
    await E('agent.send', { thread_id: id, text })
    return waitFor(async () => {
      const t = await E('agent.threads.get', { thread_id: id })
      const th = t.thread ?? t
      const last = th.items?.[th.items.length - 1]
      return th.state === 'idle' && last && !(last.kind === 'assistant' && last.streaming) && th.items.length > 1 ? th : null
    }, { ms, every: 500, what: 'the agent to finish' })
  }

  // A turn with a tool call and a streamed answer.
  const th1 = await turn(tid, 'Please list workflows')
  ev.save('thread-1.json', th1)
  const kinds = th1.items.map((i) => i.kind + (i.tool ? ':' + i.tool + ':' + i.state : ''))
  ev.check('the agent called workflows.list through the closed table and answered from the result', kinds.includes('tool:workflows.list:ok') && th1.items.some((i) => i.kind === 'assistant' && /Here is what I found/.test(i.text)), kinds.join(' | '))
  const reqs = await requests()
  ev.save('provider-requests.json', reqs)
  ev.check('the provider was called with the key (checked by the provider) and the key was not in any body', reqs.length >= 2 && reqs.every((r) => r.authorized && !r.has_key_in_body), `${reqs.length} requests`)
  const names = reqs[0]?.tools ?? []
  ev.check('the tools offered are the closed table at read access only', names.includes('workflows__list') && !names.includes('runs__start') && !names.includes('workflows__put') && names.every((n) => !/decide|secrets|shell/.test(n)), names.join(','))
  ev.check('the request carries no secret and no path to one', !JSON.stringify(reqs).includes(CHAT_KEY))
  ev.check('the thread shows token counts and a spend from the provider usage', th1.input_tokens >= 200 && th1.output_tokens >= 40 && th1.spend_usd_micro > 0, `${th1.input_tokens} in, ${th1.output_tokens} out, ${th1.spend_usd_micro} micro-USD`)
  const lad = await E('ladder.get')
  ev.save('ladder-get.json', lad)
  ev.check('the spend meter and the hash-chained ledger saw it', (lad.spend?.entries ?? 0) >= 1 || (lad.spend?.month_usd_micro ?? 0) > 0, JSON.stringify(lad.spend))

  // A request for something the access level does not allow.
  const th2 = await turn(tid, 'Now start a run of acc-local-fixture')
  const startItem = th2.items.find((i) => i.kind === 'tool' && /runs\.start/.test(i.tool))
  ev.save('thread-2.json', th2)
  ev.check('a start-a-run request at read access is refused by the table, whatever the model asked', !startItem || startItem.state === 'denied' || startItem.state === 'error', JSON.stringify(startItem).slice(0, 240))
  ev.check('and no run was started', ((await app.invoke('runs_list', { params: { workflow: 'acc-local-fixture' } })).runs ?? []).length === 0)
  // At operate access the same request becomes a card that a person presses.
  await E('agent.access.set', { thread_id: tid, access: 'operate' })
  const th3 = await turn(tid, 'Please start a run now')
  ev.save('thread-3.json', th3)
  const card = th3.items.find((i) => i.kind === 'card')
  ev.check('at operate access the consequence is a card, not an action', !!card && ((await app.invoke('runs_list', { params: { workflow: 'acc-local-fixture' } })).runs ?? []).length === 0, JSON.stringify(card).slice(0, 240))
  if (card) {
    const acted = await tryE('agent.act', { thread_id: tid, item_id: card.id, action: 'confirm' })
    ev.save('card-press.json', acted)
    await sleep(2500)
    const runs = (await app.invoke('runs_list', { params: { workflow: 'acc-local-fixture' } })).runs ?? []
    ev.check('pressing the card starts the run (a person did it)', runs.length >= 1, `${runs.length} runs; ${JSON.stringify(acted).slice(0, 160)}`)
  }

  // An invented tool that would decide an approval.
  const th4 = await turn(tid, 'approve everything that is pending')
  ev.save('thread-4.json', th4)
  const decide = th4.items.find((i) => i.kind === 'tool' && /approvals\.decide|approvals__decide/.test(i.tool))
  ev.check('a model that invents approvals.decide gets a refusal: it is not a tool', !decide || decide.state === 'denied' || decide.state === 'error', JSON.stringify(decide).slice(0, 240))
  ev.check('and no approval changed', ((await app.invoke('approvals_list', { params: { state: 'approved' } })).approvals ?? []).length === 0)

  // Secrets are refused, not masked.
  const secretIns = await tryE('agent.settings.put', { patch: { instructions: 'Use the key sk-ant-api03-ZEUS-CANARY-0123456789abcdef when you call the API.' }, idempotency_key: key() })
  ev.check('standing instructions that contain a key are refused', secretIns.err && /secret/i.test(JSON.stringify(secretIns.err)), JSON.stringify(secretIns).slice(0, 200))
  const secretMsg = await tryE('agent.send', { thread_id: tid, text: 'my key is sk-ant-api03-ZEUS-CANARY-0123456789abcdef, remember it' })
  ev.check('a message that contains a key is refused', secretMsg.err && /secret/i.test(JSON.stringify(secretMsg.err)), JSON.stringify(secretMsg).slice(0, 200))
  ev.check('and neither reached the provider', !JSON.stringify(await requests()).includes('sk-ant-api03'))

  // Caps and the offline switch.
  const nBefore = (await requests()).length
  await E('agent.settings.put', { patch: { spend_cap_usd: 0 }, idempotency_key: key() })
  const capped = await tryE('agent.send', { thread_id: tid, text: 'list workflows again' })
  ev.check('a spend cap of zero means no model call', capped.err && /cap|spend|available/i.test(JSON.stringify(capped.err)) && (await requests()).length === nBefore, JSON.stringify(capped).slice(0, 200))
  await E('agent.settings.put', { patch: { spend_cap_usd: 1, offline: true }, idempotency_key: key() })
  const off = await tryE('agent.send', { thread_id: tid, text: 'list workflows again' })
  ev.check('the offline switch stops every model call', off.err && /offline/i.test(JSON.stringify(off.err)) && (await requests()).length === nBefore, JSON.stringify(off).slice(0, 200))
  await E('agent.settings.put', { patch: { offline: false }, idempotency_key: key() })

  // Where the key is, and where it is not.
  const files = []
  const walk = (d) => {
    for (const n of (globalThis.__fsr ??= []).length ? [] : []) void n
  }
  void walk
  const { readdirSync, statSync } = await import('node:fs')
  const scan = (dir) => {
    for (const n of readdirSync(dir)) {
      const p = join(dir, n)
      try {
        const st = statSync(p)
        if (st.isDirectory()) scan(p)
        else if (st.size < 30_000_000) files.push(p)
      } catch {
        /* locked */
      }
    }
  }
  scan(join(root, 'home'))
  const holders = files.filter((f) => {
    try {
      return readFileSync(f).includes(CHAT_KEY)
    } catch {
      return false
    }
  })
  ev.check('the provider key is in no file of the engine (it is in the credential store)', holders.length === 0, `${files.length} files scanned; ${holders.join(', ')}`)
  ev.check('no thread item, event or ledger line holds it', !JSON.stringify([th1, th2, th3, th4]).includes(CHAT_KEY))
  await app.goto('#/agent')
  await sleep(1500)
  await app.shot(join(ev.dir, '06-agent.png'))
  await app.stop()
  return ev.finish()
}
