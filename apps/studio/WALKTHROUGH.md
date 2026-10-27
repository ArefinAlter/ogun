# Ogun Studio: the walkthrough

Ogun Studio runs in three ways. Since release 0.0.1b the **desktop app is standalone** (the first row below); the hosted
hub is the second, and the third is the offline showcase this walkthrough was first written for.

| Where | What answers | How to start |
|---|---|---|
| **Desktop app (0.0.1b)** | Its own `ogund`, bundled and supervised by the app, on a private pipe with a per-launch secret. Local is the default environment; no account, no network, no daemon to start. A hub is optional (Settings, Environments, "Sign in with your browser") | Install `ogun-studio-0.0.1b-windows-x64-setup.exe` (or unzip the portable build), or the Linux AppImage/.deb, from the [release](https://github.com/ArefinAlter/ogun/releases/tag/v0.0.1b). Open it and choose a sample project. |
| **Hosted hub** | The real `ogund` and `ogun-server` on sample data, plus the pre-alpha service for the screens `ogund` does not implement yet ([`deploy/pre-alpha`](../../deploy/pre-alpha/README.md)). It still runs 0.0.1a | Open `https://ogun.103.191.50.32.sslip.io` and sign in with the sample account shown on the sign-in page. One shared workspace, reset every night at 03:00 UTC. The desktop app signs in to it only after the upgrade in `deploy/pre-alpha/native-signin` is applied. |
| **Offline build** | A script inside the page (`web/src/demo`), no daemon | `npm run demo` in `apps/studio/web`, or the desktop build `npm run build:desktop-demo:app`. |

## What is real and what is prepared

On the **hosted hub**:

| Real | Prepared |
|---|---|
| Workflows, versions and hashes, runs, node instances, partitions and backfills, schedules, the event stream, approvals (read), artifacts and lineage: the daemon's own, from `ogund` through `ogun-server` | The sample data: the workflows in `deploy/pre-alpha/seed` fetch files from sample file servers inside the hub (`files.example.org`, `weather.example.org`; the weather service is down on purpose, so `ridership-forecast` fails the way an outage does) |
| Sign-in (OIDC through Dex, sessions, CSRF), roles: the sample account is an editor | The agent's words: no model is called. Every tool it calls goes to the daemon with an API token that acts as an agent |
| The approval rules: the two pending code changes (rebalance-plan v2, district-report) wait for a person at the host's terminal; no browser or token can decide them | Connections, run defaults, the provider ladder, sources, evidence, storage rules and the archive: kept by the pre-alpha service per browser for 24 hours; they do not change the daemon yet |

What it cannot do: run code operators (no operator sandbox is installed, and the workflow page says so), reach the
internet (workflows see only the sample file servers), or decide an approval from a browser (remote approvals are off,
ADR 0010). Agent threads and settings are per browser; what you store on the daemon is visible to every visitor until
the nightly reset.

In the **offline build**, everything on screen is the real Studio frontend and the data comes from the script. The
interface does not say so anywhere you look, so say it yourself: the numbers are sample data, the agent's answers are
prepared, and the connector catalogue is a list of what Ogun would reach, most of it not run yet (each card says
"Manifest only" or "Built"). The About page states the same.

| Real | Prepared |
|---|---|
| Every page, control and rule of Studio: the approval flow with its arming delay and request-hash binding, the facts-first layout, the untrusted-text blocks, the partition strip, the DAG | The daemon: runs advance on a 3 s clock, the stream seals a window every 30 s, decisions take effect instantly |
| The agent screen's rules: access levels, the closed tool table, a refused call, the cards, the rule that nothing in a conversation decides an approval | What the agent says. No model is called: replies come from a script. The tools it calls are real reads of the scripted daemon |
| The six workflow documents and their hashes (what `ogun hash` prints; `examples/bikeshare`) | Run ids, artifact digests, station readings, station ids, the edge keys |
| The operator code hashes in the approval (the real BLAKE3 of `rebalance.py` and `rebalance_weather.py`) | The reason text the agent attached to a request |

The runtime itself is proven on Linux (`docs/IMPLEMENTATION_STATUS.md`): the O1 exit test kills `ogund` during a
121-partition backfill and resumes only the unfinished partitions, and the stream twin re-seals a window with no lost
or duplicated record. In the offline build two scenario controls, in the command palette (Ctrl+K, type "scenario"),
replay exactly those two claims.

## On the standalone desktop app (fifteen minutes, 0.0.1b)

Everything below runs on your computer against the app's own engine. The checks behind each step are in
[`docs/ACCEPTANCE-0.0.1b.md`](../../docs/ACCEPTANCE-0.0.1b.md).

1. **First start.** The app opens on Local with a short waiting page while its engine starts, then offers two sample
   projects. Import the starter: the workflows arrive with their real hashes (`earthquakes-daily` reads the public USGS
   catalogue; `github-releases` reads this repository's releases). Nothing leaves the machine until you run one.
2. **Run it.** Overview, a workflow, Run. Core operators (`acquire`, `parse`, `filter`, `partition`, `hash`,
   `manifest`) run on the real input; the run page shows nodes, artifacts and their lineage.
3. **Backfill and kill it.** Partitions, a range (121 days is the acceptance size), Backfill. End `ogund` from Task
   Manager, or end the whole app: start it again and the engine resumes the unfinished partitions and only those. Ask for
   the range again and no duplicate work starts.
4. **Schedules and streams.** Settings, Schedules: a cron fires while the app is open (the app is not a service; it does
   not run while closed). Streams: a file-tail source sealing one-minute windows once every key has advanced.
5. **Approvals.** A workflow with code or an export waits for you: facts first, the code digest, an arming delay, your
   decision bound to the request hash. The engine only accepts it from the pinned `ogun-studio.exe` at the other end of
   the pipe; the `ogun` CLI and an agent are refused.
6. **Code operators.** After you approve the exact hash, Python (the 3.13 that ships in the installer) runs in the
   Windows AppContainer: no network, no files beyond its inputs and scratch. Settings, Security says what it does and does
   not isolate (`docs/windows-sandbox.md`).
7. **The agent.** Settings, Providers: pick a provider (OpenAI-compatible, Anthropic, Gemini or Ollama), type your key
   (masked, sent once, kept in the credential store) and consent. "What will be sent" shows the next request. At Read
   only the agent lists and explains; at Operate it starts a run you asked for. Nothing in a conversation decides an
   approval. No live provider was called in the acceptance run; the four wire formats were run against local servers.
8. **Exporters and connections.** Connections: a folder, a SQLite file or an S3-compatible bucket, with secrets by name.
   Test shows what was checked; each delivery is an approval.
9. **Settings, evidence, storage.** The evidence bundle lists what it holds; storage rules show what a cleanup would
   remove before it removes it.
10. **Benchmarks.** Benchmarks runs the sample suite (scripted agents, network denied, hash-chained trajectories) and
    lets you replay a trial. It checks the harness, not a model.
11. **Sign in to a hub (optional).** Environments, Add hub, Sign in with your browser: authorization code with PKCE and a
    loopback redirect, tokens in the credential store. The hosted pre-alpha hub accepts this only after its owner applies
    `deploy/pre-alpha/native-signin`; it was verified against a local hub with an identity-provider stand-in.

## On the hosted hub (ten minutes)

1. **Sign in.** The sign-in page shows the sample account. After Dex, Studio opens on the agent.
2. **The agent.** "What is running, and what is waiting for a person?": the agent calls `status.get`, `runs.list`,
   `approvals.list` on the real daemon (open "Worked, n tool calls" to see each answer) and lists the two pending code
   approvals. It can read them; it cannot decide them. "Backfill trips-backfill since 2019": it reads the partitions
   (36 months done since 2023-01) and plans the missing months; press the button and the backfill runs for real, as you.
3. **Overview and Partitions.** Six workflows; `trips-backfill` shows the backfilled months green. Runs are real runs of
   core operators fetching the sample archive.
4. **A failure.** "Why did the ridership forecast fail last night?": the forecast node failed because the sample weather
   service answered 503; the agent reads the run and offers a retry for you to press.
5. **Approvals.** The `code change` for rebalance-plan v2 and district-report: facts first, the code digests, and a note
   that a person decides at the host's terminal. Open the workflow: the page says this hub cannot run code nodes.
6. **Artifacts and lineage.** The latest rebalance-plan run: the shortlist CSV preview and its OpenLineage events.
   "Which stations are likely to run empty today?" reads the same artifact.
7. **Settings.** Schedules are the stored triggers; Environments shows this hub and its bridges as they are (sandbox not
   installed, remote approvals locked). Connections, Providers and Storage keep what you set for 24 hours.

## The offline flow (twelve minutes)

The scenario looks best after it has run for a minute or two.

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
   101 systems (BigQuery, Snowflake, Airflow, Datadog, S3, Alibaba OSS, DingTalk ...). Open one: auth method, hosts that
   would need an egress approval, the operators it would add. Add a connection: settings and the *name* of a secret,
   never a value. "Test" checks settings, the secret's presence and the host list, and says "not attempted" for the part
   that would leave the machine. In the connection editor "Check again" re-reads the secret names; here it also marks the
   secret as stored, so the failing connection turns green.
9. **Close (30 s).** Press Ctrl+K: every page, workflow, run and approval is one keystroke away. Then the honest line:
   the runtime passes its exit test on Linux; Studio against the real daemon, Android, iOS and the real MQTT brokers are
   the next items on `docs/IMPLEMENTATION_STATUS.md`.

## What the connector catalogue does and does not claim

Of 101 connectors, 31 are `built`: the orchestrator packages, the edge ingest sources, the editor and agent surfaces, the
15 `ogun-export` sinks, and the local files and SQLite connections, written from public documentation and checked against
local fakes. The other 70 are
`contract`: a manifest of what Ogun would connect to, with no operator behind it. None is `verified` against the real
service. Say this before anyone asks. Vendor domains, ports and regions are claims to check (O-16 section 9).

## The hub on loopback

`deploy/demo-hub` is the loopback-only variant of the hosted hub (no public address, one static user, reached by an SSH
tunnel); `deploy/pre-alpha` replaced it on the shared host. Both run the real `ogund` and `ogun-server`, neither has an
operator sandbox. The hosted hub still runs 0.0.1a.

## If asked for proof on the command line

The CLI builds on Windows (`cargo build -p ogun-cli`, binary at `target\debug\ogun.exe`) and validates the sample
workflows: `ogun validate examples/bikeshare/rebalance-plan.workflow.json`, `ogun diff` of v1 against v2 (the new version
needs an approval because `estimate`'s code changed), `ogun partitions` on the trips backfill (93 months), `ogun next`,
and the validator refusing a password typed into a workflow (`ir.secret_inline`). The daemon also runs on Windows
since 0.0.1b: operators run in an AppContainer, and the desktop app decides approvals through the pinned `ogun-studio.exe`
pipe peer; terminal (tty) approvals with the CLI are still a Linux story.

## Known seams in the offline build

- The scenario is in memory: a reload resets it, and two tabs see two different daemons.
- The backfill finishes in about four minutes; after that only the stream keeps moving. Reload for a fresh run.
- Decisions are recorded as "operator"; there is no sign-in.
- Validation in the import box checks the obvious (JSON, integers, inline secrets, unknown operators, missing code
  hashes) and says "valid" for anything else; the real validator is stricter.
- Settings changes (schedules, connections, run defaults, storage rules) live in the script and reset on reload.
  Appearance and keybindings persist on the desktop app and the local build, and in memory on the hosted copy.
- The agent answers six kinds of request (status, backfill, a failure, a draft, stations at risk, connectors) plus
  approvals and "explain a workflow". Anything else gets the list of what it can do.

## The desktop app's own engine (0.0.1b)

The Tauri desktop build is no longer a window onto a daemon you start by hand. It ships `ogund` beside itself, starts
it with its own profile and a per-launch credential, and shows the real data. The frontend sees it as follows
(`web/src/api/tauri.ts`, `localMap.ts`, `views/Engine.tsx`, `views/Welcome.tsx`):

- **Engine state.** `studio_info` carries `engine` (`starting`, `running`, `restarting`, `failed`, `stopped`,
  `external`). While it is not running the page is replaced by a waiting page (not a page of errors); a failure shows
  the cause, the end of the log and a Restart button. When the engine becomes ready the whole page remounts so every
  view loads again.
- **Everything beyond the closed typed commands** (agent threads, connections, run defaults, the provider ladder,
  sources, storage, archive, environments) goes through one relay, `engine_call`, with the daemon's own method
  names. `localMap.ts` is the only place the hosted service's names are translated, and `tests/localmap.test.mjs`
  pins it.
- **First run.** A workspace with no workflow offers the bundled sample projects (`samples_list`, `sample_import`).
- **Keys.** A provider key or a connection secret is typed into a masked field and sent once with `secret_put`. The
  value is cleared from the page at once, is never in storage or a URL, and is never shown again. The agent stays
  closed until a provider has a model, a stored key and the person's consent, offline is off and both caps are above
  zero (`lib/agentGate.ts`, tested), and "What will be sent" shows the next request before anything leaves.
- **Hubs.** `hubs.json` in the app's data folder lists the environments; a hub signs you in with the browser (`hub_signin_start`),
  the shell exchanges the ID token for an `ogs1_` session at the hub's `POST /auth/native`, keeps the tokens in the
  credential store and refreshes them on a 401. Studio never decides a hub approval; that stays with the hub's own
  trust surface.
- **Still scripted.** Only the scripted build (`npm run demo`) and the hosted pre-alpha hub answer from prepared
  sessions; the desktop app does not.
