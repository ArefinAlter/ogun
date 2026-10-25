# Ogun Studio: the walkthrough

A scripted walk through Ogun Studio on a bike-share network's sample workflows (`examples/bikeshare`). Everything on
screen is the real Studio frontend; the data comes from a script inside the page (`web/src/demo`), not from a daemon.
The interface does not say so anywhere you look, so say it yourself at the start: the numbers are sample data, the
agent's answers are prepared, and the connector catalogue is a list of what Ogun would reach, most of it not run yet
(each card says "Manifest only" or "Built"). The About page states the same.

## Start it

| Where | How |
|---|---|
| Hosted, no install | `https://ogun-site.vercel.app/studio/`, served by the `ogun-site` project under its strict CSP. It is `noindex`, keeps nothing in the browser (preferences live in memory) and has no sign-in. To update it: `npm run build:site`, then `node scripts/publish-site.mjs ../../../../ogun-site`, then commit in `ogun-site`; Vercel deploys on push. |
| Desktop app | `npm run build:desktop-demo:app` in `apps/studio/web` builds the Tauri shell on the scripted backend and copies it to `dist/desktop/ogun-studio.exe`. The window has no operating-system title bar: the top row is the title bar, with minimize, maximize and close at the right. Windows needs the WebView2 runtime, which Windows 11 has. `ogun-studio-daemon.exe` in the same folder is the build that links to a real daemon or hub. |
| In a browser, locally | `npm run demo` (http://localhost:5173), or `npm run build:demo && npm run preview:demo` (http://localhost:4174). Reload to start the script again. |

The scenario looks best after it has run for a minute or two.

## What is real and what is prepared

| Real | Prepared |
|---|---|
| Every page, control and rule of Studio: the approval flow with its arming delay and request-hash binding, the facts-first layout, the untrusted-text blocks, the partition strip, the DAG | The daemon: runs advance on a 3 s clock, the stream seals a window every 30 s, decisions take effect instantly |
| The agent screen's rules: access levels, the closed tool table, a refused call, the cards, the rule that nothing in a conversation decides an approval | What the agent says. No model is called: replies come from a script. The tools it calls are real reads of the scripted daemon |
| The six workflow documents and their hashes (what `ogun hash` prints; `examples/bikeshare`) | Run ids, artifact digests, station readings, station ids, the edge keys |
| The operator code hashes in the approval (the real BLAKE3 of `rebalance.py` and `rebalance_weather.py`) | The reason text the agent attached to a request |

The runtime itself is proven on Linux (`docs/IMPLEMENTATION_STATUS.md`): the O1 exit test kills `ogund` during a
121-partition backfill and resumes only the unfinished partitions, and the stream twin re-seals a window with no lost
or duplicated record. Two scenario controls, in the command palette (Ctrl+K, type "scenario"), replay exactly those two
claims.

## The twelve-minute flow

0. **The agent (2 min).** The app opens on a conversation. Click "What is running, and what is waiting for a person?":
   the agent calls tools one by one (a folded "Worked, 4 tool calls" line that opens to each call and what the daemon
   answered), then answers and shows the two pending approvals as a card. "It can read them. It cannot decide them:
   there is no such tool, at any access." Then "Draft a daily workflow ...": the proposal card is already validated,
   shows its hash and what waits for a person (an egress approval), and the thread is Read only, so the agent says it
   cannot store it. Press **Store as you**: the workflow is in the daemon, stored by you, not by the agent. Open the
   access picker and show Author and Operate; start a new thread at Operate, ask for the trips backfill, and the agent
   starts the run itself, while an Author thread leaves that press to you. Settings, Agent, switch off `backfills.plan`
   and ask again: the call is refused and the thread says why.
1. **Overview (30 s).** "Five workflows, each version stored by hash. Two runs active, two decisions waiting for a
   person." Point at the active-run meter: a seven-year trip-archive backfill, month by month, advancing live.
2. **Partitions (1 min).** Choose `trips-backfill`. The strip and the calendar: green months are done and will never run
   again; the ember ones are in flight. "A crash resumes only what was unfinished."
3. **Kill it (1 min).** Ctrl+K, "Kill ogund (SIGKILL) and restart it". Two toasts: stopped, then started. Open the
   backfill run: the finished months are untouched, the in-flight attempts show attempt 2 with cause `daemon_restart`,
   and the meter keeps climbing. This is the O1 exit test, in pictures.
4. **The stream (1 min).** Streams, then `docks`. One-minute windows sealed with a digest; a watermark; a backlog that
   is never dropped. Ctrl+K, "Deliver late station data": the window becomes revision 2 and a run recomputes it. "Late
   data is a new revision, never a silent gap."
5. **The approval (2 min).** Approvals. The `code change` for `rebalance-plan` v2: an agent added a weather adjustment
   and asked. Facts first, in a fixed order, with the old and new code digests; the agent's reason sits in a dashed box
   marked "not verified by Ogun". Click Approve: the confirmation names the decision, arms after 1.5 s, and binds to the
   request hash. Approve it. A run of v2 starts; the sidebar dot turns green. "Nothing an agent can reach decides this.
   Only a person, on a trust surface."
6. **The district report (1 min).** The second approval is a `publish`: 12 PDFs in English and Spanish, 38 addresses,
   waiting since the 2nd. "Nothing reaches a district office until a person says so." Open the run from the approval;
   the `deliver` node is `waiting_for_user`.
7. **Artifacts and lineage (1 min).** Runs, filter `rebalance-plan`, the latest run, "Artifacts and lineage". The CSV
   preview is the shortlist: station, area, bikes, empty risk and when it is expected to be empty. The Lineage tab is
   OpenLineage: every artifact knows the operator and inputs that made it.
8. **Settings and connectors (2 min).** Ctrl+comma. Every section follows T3 Code's settings, mapped to Ogun:
   Appearance (six themes, a custom theme from three colours, contrast, density, glass), Keybindings (record a chord,
   conflicts are flagged), Providers (the `llm.json` ladder, with DeepSeek, Qwen, Kimi, GLM, Doubao and the Western
   providers as presets, validated by the O-03 rules), Evidence (what a downloaded evidence bundle holds), Schedules (a
   cron is read back in words and checked against `ogun next`), Workflow sources, Storage (a cleanup shows what it
   would remove before it removes it), Environments and Security (fixed rules are shown, not hidden). Then Connectors:
   99 systems (BigQuery, Snowflake, Airflow, Datadog, S3, Alibaba OSS, DingTalk ...). Open one: auth method, hosts that
   would need an egress approval, the operators it would add. Add a connection: settings and the *name* of a secret,
   never a value. "Test" checks settings, the secret's presence and the host list, and says "not attempted" for the part
   that would leave the machine. In the connection editor "Check again" re-reads the secret names; here it also marks the
   secret as stored, so the failing connection turns green.
9. **Close (30 s).** Press Ctrl+K: every page, workflow, run and approval is one keystroke away. Then the honest line:
   the runtime passes its exit test on Linux; Studio against the real daemon, Android, iOS and the real MQTT brokers are
   the next items on `docs/IMPLEMENTATION_STATUS.md`.

## What the connector catalogue does and does not claim

Of 99 connectors, 29 are `built`: the orchestrator packages, the edge ingest sources, the editor and agent surfaces, and
the 15 `ogun-export` sinks, written from public documentation and checked against local fakes. The other 70 are
`contract`: a manifest of what Ogun would connect to, with no operator behind it. None is `verified` against the real
service. Say this before anyone asks. Vendor domains, ports and regions are claims to check (O-16 section 9).

## Showing the real daemon, not the script

`deploy/demo-hub` runs the real `ogund`, `ogun-server` and a sign-in provider on a Linux host, on loopback only. From the
presenter's machine:

1. Open the tunnel: `ssh -N -L 18443:127.0.0.1:18443 -L 18444:127.0.0.1:18444 <user>@<host>`.
2. Browser: `http://localhost:18443/`, sign in with the account. This is Studio-web on a real daemon: the runs,
   partitions and node instances are the daemon's own. `hub-backfill` is a 45-month backfill of core operators that
   really ran; `hub-heartbeat` is a schedule that fires every 15 minutes.
3. Desktop: create an API token in Studio-web, then Settings, Environments, "Add environment" in
   `ogun-studio-daemon.exe`. The token acts as an agent, so the link is read-only.

Say plainly what it is not: one machine, one static user, no operator sandbox in the image (so workflows with Python or
SQL operators do not run there), and it is reached by a tunnel, not a public address. The hub does not run agent
threads yet, so its Studio hides the Agent screen.

## If asked for proof on the command line

The CLI builds on Windows (`cargo build -p ogun-cli`, binary at `target\debug\ogun.exe`) and validates the sample
workflows: `ogun validate examples/bikeshare/rebalance-plan.workflow.json`, `ogun diff` of v1 against v2 (the new version
needs an approval because `estimate`'s code changed), `ogun partitions` on the trips backfill (93 months), `ogun next`,
and the validator refusing a password typed into a workflow (`ir.secret_inline`). The daemon also builds on Windows,
but Windows has no operator sandbox and no peer inspection yet, so terminal approvals are refused there; the live
daemon story is a Linux (or WSL) story for now.

## Known seams in the script

- The scenario is in memory: a reload resets it, and two tabs see two different daemons.
- The backfill finishes in about four minutes; after that only the stream keeps moving. Reload for a fresh run.
- Decisions are recorded as "operator"; there is no sign-in.
- Validation in the import box checks the obvious (JSON, integers, inline secrets, unknown operators, missing code
  hashes) and says "valid" for anything else; the real validator is stricter.
- Settings changes (schedules, connections, run defaults, storage rules) live in the script and reset on reload.
  Appearance and keybindings persist on the desktop app and the local build, and in memory on the hosted copy.
- The agent answers six kinds of request (status, backfill, a failure, a draft, stations at risk, connectors) plus
  approvals and "explain a workflow". Anything else gets the list of what it can do.
