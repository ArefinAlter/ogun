# Acceptance run for Ogun 0.0.1b

What was run, how, what passed, and what is only partly verified. Written by the agent that built the release; the
evidence is the output of `tools/acceptance/` (scenario scripts, a fixture server, an identity-provider stand-in and a
hub stack), kept in `D:\verne-build\ogun\evidence\<scenario>\` on the build machine and summarised here. Nothing in this
file is a claim the evidence does not back; where something was checked against a stand-in rather than the real service, the
row says so.

## Method

- **The real app.** Each scenario starts the real `ogun-studio.exe` (the Tauri shell with its bundled `ogund` and the other
  sidecars) on a scratch profile (`OGUN_HOME`, `OGUN_STUDIO_DATA`, `WEBVIEW2_USER_DATA_FOLDER` under a scratch folder), so
  the owner's real app data is never read or written. The harness drives the app's window through the WebView2 remote
  debugging port (Playwright over CDP) and calls the engine through the app's own commands (webview, shell, named pipe,
  `ogund`), the path the screens use. It takes screenshots of the real window.
- **Real inputs.** A local fixture server (`fixture_server.py`, loopback, dual stack, `fixture.localtest.me`) stands in
  for HTTP sources, an S3-compatible store that checks AWS Signature V4, and model endpoints in four wire formats
  (OpenAI-compatible, Anthropic Messages, Gemini, Ollama). The public GitHub REST API and the USGS earthquake catalogue are
  used for real in scenario 2.
- **Stand-ins, named.** The identity provider in scenario 7 is `idp_mock.py`, which behaves like Dex for a public client
  (authorization code with PKCE S256, loopback redirect, refresh-token rotation, ES256 ID tokens). It is not Dex. The hub is a
  real `ogun-server` with a real `ogund`. The model providers in scenario 6 are local servers that speak each provider's wire format
  as its public documentation describes it. No model API key exists on the build machine, so **no live provider was called**.
- **One hook.** Scenario 9 makes runs look 40 days old with `OGUN_STORAGE_CLOCK_OFFSET_DAYS=40` so the retention rules have
  something to act on; the removal that follows is real (bytes leave the artifact store on disk).
- **Machine.** Windows 11 Pro, 16 logical CPUs shared with other builds, so timings in the evidence are slow and not a
  benchmark.

## Results

### The ten scenarios

Run against the **installed release build** (the NSIS installer, run silently into a scratch folder; engine version
`0.0.1-beta`) on 2026-10-27, one scenario after another. The run was made twice. A first pass used an earlier installer
(built shortly before commit `957ec5a`; all ten scenarios passed). The pass reported here used the installer that is published, built
from commit `28e214b`; the commits after it change documentation and release packaging text only. In the final pass the
GitHub check of scenario 2 first failed with `http.status_403`: the public API allows 60 unauthenticated requests an hour
per address and this address's allowance was used up (the address is shared with other work). The evidence of that attempt is kept
(`docs/acceptance-0.0.1b/s2-first-attempt-http403.json` and `s2b-first-attempt-http403.json`), and scenarios 2 and 2b were run again once the limit had reset. The numbers in the table
are from the final pass. "Passed" means every check of the scenario passed; "with limits" says what the
checks stand in for. The files `docs/acceptance-0.0.1b/<scenario>.json` hold each scenario's checks as the run wrote them; four
screenshots from the run are in `docs/images/acceptance-0.0.1b-*.png`.

| # | Scenario | Result | What stands in for the real thing, and what was not covered |
|---|---|---|---|
| 1 | Install, launch, empty workspace, sample import with real hashes | Passed (16/16); also passed from the portable zip (16/16) | Silent install into a scratch folder with a scratch profile; SmartScreen and a double-click install were not exercised. The engine reported `0.0.1-beta`. |
| 2 | Core operators on real inputs; 121-partition backfill; kill `ogund`; kill the whole app | Passed (19/19 and 6/6) | Real APIs: GitHub releases (this repository) and the USGS earthquake catalog. The fixture server stands in for the rest. Open-Meteo, used by the 0.0.1b draft of the starter project, was dropped because its `robots.txt` disallows everything and `acquire` obeys it. After a kill the engine resumes unfinished runs itself; asking for the range again starts no duplicate work (a fix made during this acceptance). |
| 3 | Schedules while the app is open; streams with a local producer | Passed (14/14) | Cron every minute and one file-tail stream. MQTT, LoRaWAN, Modbus and HTTP-push sources were not run. |
| 4 | Approvals in Studio, facts first, arming delay, request-hash binding, channel evidence | Passed (15/15) | The Windows peer check (image path of the pinned `ogun-studio.exe` at the other end of the pipe) is what admits the Studio channel. No program at the pinned path other than Studio itself was run, and the engine's credential lives only in the app's memory, so "another program with the right path" was not tried; the `ogun` CLI (no credential on that engine) and the webview's relay (`approvals.decide` is not on its list) were shown refused. |
| 5 | Code operators in the Windows sandbox | Passed (13/13) | Python 3.13.9 as shipped in the installer. Linux (bubblewrap) was not re-run in this acceptance. macOS has no sandbox. |
| 6 | Agent threads with BYOK providers | Passed with limits (33/33) | **No live provider was called** (no key on the machine). Four wire formats were exercised against local servers written from each provider's public documentation: OpenAI-compatible, Anthropic Messages, Gemini `streamGenerateContent` and Ollama `/api/chat`. Mistral, DeepSeek, Qwen, Kimi and GLM use the OpenAI-compatible path with their base URL and were not exercised separately. |
| 7 | Hub sign-in from Studio desktop | Passed with limits (35/35) | A real `ogun-server` and `ogund` on this machine; the identity provider is `idp_mock.py`, which behaves like Dex for a public client, **not Dex**. The hosted hub was **not** upgraded and not touched (no access to its host): desktop sign-in to it works only after the owner applies `deploy/pre-alpha/native-signin`. Plain HTTP on loopback only; TLS was not exercised. |
| 8 | Exporters and connections | Passed (19/19) | The S3-compatible store is a local server that verifies AWS Signature V4; no real S3, MinIO or other service was used. SQLite and folder deliveries are real. |
| 9 | Settings, evidence bundle, storage rules and cleanup | Passed (16/16) | Runs were made to look 40 days old with `OGUN_STORAGE_CLOCK_OFFSET_DAYS=40`; the removal of bytes from the artifact store is real. Keybinding recording was not driven; the appearance setting and its persistence were. |
| 10 | Benchmarks from the app | Passed (13/13) | The sample suite only: scripted agents, not models. A Docker runtime also answered on this machine but was not used. Harbor, Spider 2.0-DBT and ADE-bench adapters exist in the engine and are not offered in Studio. |

Beyond the ten scenarios: the published Linux AppImage was started headless (Xvfb, Ubuntu 22.04 under WSL) from a scratch
home: the window opened and a screenshot was taken, its own `ogund` and `ogun-ingest` started, and the engine stopped when
the app was closed. The `.deb` was inspected (`dpkg -I`, `dpkg -c`). No scenario was driven through the Linux build.

### What the acceptance run found and fixed

- `runs.start` for a range started partitions that another run was already working on (a backfill resumed after a restart, then
  asked for again): they ran twice. Partitions in flight are now left to their run.
- The Windows named pipe of one folder had a different name depending on how the folder was spelled (`D:/x` and `D:\x`), so a
  hub's `ogun-server` could not find its `ogund`. One folder is now one pipe.
- `acquire` treated the transport's occasional `http.protocol_error` against `api.github.com` as permanent; it is retryable now.
- The benchmark helper could not find the Windows sandbox because it started with too few environment variables.
- The starter project's weather workflow could never run (Open-Meteo's `robots.txt`); it is replaced by a USGS one.
- Importing a sample project twice reported `created: true` for versions that already existed.
- `agent.preview` was missing from the shell's relay list; the `local-files` and `sqlite` connector manifests did not exist.
- The Studio storage cleanup was only unit-checked before; it now ran for real.

### Every check

### s1: 16 of 16 checks passed

| Check | Result |
|---|---|
| the app owns a running engine | pass |
| exactly one ogund child of the app | pass |
| the engine takes no secret on its command line | pass |
| the engine listens on no TCP port (a named pipe only) | pass |
| the link is the local engine and it answers | pass |
| the workspace starts empty | pass |
| the bundled samples are offered | pass |
| the sample project is imported from the first-run screen | pass |
| the sample import reports workflows with BLAKE3 workflow hashes | pass |
| each hash the engine stored equals the hash `ogun hash` computes from the file (JCS plus BLAKE3) | pass |
| importing again creates nothing new | pass |
| workflows.list shows each of them by name | pass |
| closing the app stops the engine | pass |
| and the app itself | pass |
| a second launch starts a new engine | pass |
| the workspace survived the restart | pass |

Screenshots: 01-empty-workspace.png, 01b-after-sample-button.png, 02-after-sample-import.png, 03-second-launch.png

### s2: 19 of 19 checks passed

| Check | Result |
|---|---|
| acquire, parse, filter, partition, hash and manifest run to success on a local fixture | pass |
| the run produced artifacts | pass |
| the bytes of every artifact hash to its BLAKE3 digest (checked with the ogun CLI) | pass |
| parse turned the JSON array into 10 rows | pass |
| filter dropped the one draft | pass |
| lineage events (OpenLineage) were recorded for the run | pass |
| the GitHub releases workflow runs against the real API | pass |
| the fetched document is GitHub JSON that names the v0.0.1a release | pass |
| the plan covers 121 partitions | pass |
| the app restarted the engine by itself | pass |
| some partitions finished before the kill and some did not | pass |
| asking for the range again right after the restart starts no duplicate work | pass |
| all 121 partitions are done | pass |
| every partition was fetched, and only the ones in flight at the kill more than once | pass |
| no partition that was done before the kill ran again | pass |
| a further backfill has nothing to do | pass |
| the app and its engine are gone | pass |
| after the whole app was killed, the backfill finishes on the next launch | pass |
| all 121 days of the second range were fetched, only those in flight at the kill more than once | pass |

Screenshots: 10-after-app-kill.png

### s2b (second part of scenario 2): 6 of 6 checks passed

| Check | Result |
|---|---|
| the GitHub releases workflow runs against the real API | pass |
| the fetched document is GitHub JSON that names the v0.0.1a release | pass |
| parse made one row per release | pass |
| the earthquakes workflow runs three daily partitions against the real USGS API | pass |
| three documents came back, and they are USGS GeoJSON | pass |
| the three partitions are done | pass |

### s3: 14 of 14 checks passed

| Check | Result |
|---|---|
| the scheduler started runs on its own while the app was open | pass |
| and they succeeded | pass |
| about a minute apart | pass |
| the daemon log attributes them to the scheduler, not to a person | pass |
| a paused workflow gets no new scheduled run | pass |
| the ingest bridge is supervised by the app and reports running | pass |
| three event-time windows sealed and started their runs | pass |
| and they succeeded | pass |
| the first window rolled up its five records for station S1 and no more | pass |
| a late record revised its window: a new run followed | pass |
| the revision counts six records for S1 (five and the late one) and still five for S2: the duplicate was not counted | pass |
| the ingest bridge is a child of the app | pass |
| the app restarted the ingest bridge | pass |
| records appended after the restart were ingested (a window ran again) | pass |

Screenshots: 09-streams.png

### s4: 15 of 15 checks passed

| Check | Result |
|---|---|
| importing the bikeshare project leaves approvals waiting for a person | pass |
| the daemon computed the facts (code hash, workflow hash, what it may do) | pass |
| the screen shows the facts above the decision buttons | pass |
| the confirm button is disabled while it arms | pass |
| and enabled after the arming delay | pass |
| review returns the daemon facts and an arming delay | pass |
| a confirmation before the button armed is refused | pass |
| and that confirmation is spent: it cannot be tried again | pass |
| a deny is recorded with the Studio channel | pass |
| an approve after the delay is recorded with the Studio channel and the hash it was shown | pass |
| a decided approval cannot be decided again | pass |
| the webview cannot send approvals.decide itself: it is not in the shell relay list | pass |
| another program on this account (the ogun CLI) cannot decide on the app engine: no credential | pass |
| after all of that, only the two decided approvals changed | pass |
| the daemon log records the decisions with the channel and the human credential | pass |

Screenshots: 04-approval-facts.png, 04-approvals-list.png, 04-confirm-arming.png

### s5: 13 of 13 checks passed

| Check | Result |
|---|---|
| the Windows sandbox is available and says what it does and does not isolate | pass |
| the workflow is stored but waits for approval of its code | pass |
| before approval the run does not start | pass |
| a person approved exactly that code in Studio | pass |
| the code operator ran to success inside the sandbox | pass |
| it read its input rows (10) with the bundled Python | pass |
| it wrote in its scratch folder | pass |
| it could not list the person profile folder | pass |
| it could not write a file next to the secret | pass |
| it could not read a file nobody granted | pass |
| it could not reach a loopback listener | pass |
| the canary never appears in the operator output | pass |
| changing the code makes a new version that needs a new approval | pass |

Screenshots: 05-after-code-run.png

### s6: 33 of 33 checks passed

| Check | Result |
|---|---|
| a provider is configured with the key named, not the key | pass |
| the provider is listed but not available until the person agrees to send text to it | pass |
| without consent no model call is made | pass |
| and the provider received nothing | pass |
| the person can see what would be sent before it is sent | pass |
| the agent called workflows.list through the closed table and answered from the result | pass |
| the provider was called with the key (checked by the provider) and the key was not in any body | pass |
| the tools offered are the closed table at read access only | pass |
| the request carries no secret and no path to one | pass |
| the thread shows token counts and a spend from the provider usage | pass |
| the spend meter and the hash-chained ledger saw it | pass |
| a start-a-run request at read access is refused by the table, whatever the model asked | pass |
| and no run was started | pass |
| at operate access the same request starts the run and the thread shows a run card | pass |
| and the run the agent started is the one the card names | pass |
| a model that invents approvals.decide gets a refusal: it is not a tool | pass |
| and no approval changed | pass |
| standing instructions that contain a key are refused | pass |
| a message that contains a key is refused | pass |
| and neither reached the provider | pass |
| anthropic: the thread called workflows.list and answered from the result | pass |
| anthropic: its own authentication was accepted and no key was in any body | pass |
| anthropic: usage from the stream reached the thread | pass |
| google: the thread called workflows.list and answered from the result | pass |
| google: its own authentication was accepted and no key was in any body | pass |
| google: usage from the stream reached the thread | pass |
| ollama: the thread called workflows.list and answered from the result | pass |
| ollama: its own authentication was accepted and no key was in any body | pass |
| ollama: usage from the stream reached the thread | pass |
| a spend cap of zero means no model call | pass |
| the offline switch stops every model call | pass |
| the provider key is in no file of the engine (it is in the credential store) | pass |
| no thread item, event or ledger line holds it | pass |

Screenshots: 06-agent.png

### s7: 35 of 35 checks passed

| Check | Result |
|---|---|
| the owner and a viewer enrolled through the hub invitations | pass |
| no hub is known at first | pass |
| the browser was sent to the provider with PKCE S256 and a loopback redirect | pass |
| as the public client: no client secret anywhere in the URL | pass |
| the app shows the person a short page and then needs nothing else | pass |
| the sign-in completed | pass |
| the hub says who this is and what role they have | pass |
| the hub is listed as signed in with browser sign-in | pass |
| the session and refresh tokens are in the Windows credential store | pass |
| no file of the app, its engine or its webview holds a session token | pass |
| the saved hub list holds no token | pass |
| the link is the hub, as a person | pass |
| Studio on a hub cannot decide approvals | pass |
| a workflow was stored on the hub by its operator (CLI) | pass |
| the hub link lists the hub workflow | pass |
| an owner started a run on the hub from Studio and it succeeded | pass |
| a pending approval exists on the hub and Studio can read it | pass |
| Studio refuses to decide a hub approval | pass |
| an API token (an agent) can read approvals | pass |
| an API token cannot decide one | pass |
| signing out keeps the hub in the list as signed out | pass |
| signing out removed the tokens from the credential store | pass |
| the hub ended the session (its log says so) | pass |
| and logged the native sign-in by user id, not by token | pass |
| a viewer signs in | pass |
| the hub refuses a viewer who tries to start a run | pass |
| but the viewer can read | pass |
| an identity with no account on the hub is refused, with a way forward | pass |
| after the session ended the next call worked: the app refreshed and signed in again | pass |
| the provider saw a refresh-token grant | pass |
| the hub logged a second native sign-in | pass |
| with the refresh token revoked the call fails as unauthenticated | pass |
| and the hub shows as signed out | pass |
| signing in again works | pass |
| signing out of a signed-out hub removes it from the list | pass |

Screenshots: 07-hub-viewer.png, 08-signed-out.png

### s8: 19 of 19 checks passed

| Check | Result |
|---|---|
| no connection exists at first | pass |
| the secret list holds names only | pass |
| a secret pasted into a setting is refused | pass |
| four connections are stored and show secret names only | pass |
| Test of the folder connection passes (it wrote and removed a probe file) | pass |
| Test of the SQLite connection passes (it opened the file and wrote a scratch table) | pass |
| Test of the bucket connection passes against the signature-checking server | pass |
| Test with a wrong secret fails, and says so without the secret | pass |
| the server saw the wrong signature (the test really sent a request) | pass |
| the settings page lists the connections by name | pass |
| three deliveries wait for a person, each with its own approval | pass |
| the approval facts name the connection and where it delivers, not a secret | pass |
| after the approvals the run succeeded | pass |
| the folder received the rows as a file | pass |
| the SQLite table holds the 10 rows | pass |
| the bucket received an object, and no further signature failed | pass |
| no file in the engine data (store, events, lineage, artifacts, logs, receipts) holds a secret value | pass |
| and neither do the run events | pass |
| or the lineage | pass |

Screenshots: 08-connections.png

### s9: 16 of 16 checks passed

| Check | Result |
|---|---|
| two runs succeeded and one failed on purpose | pass |
| the appearance setting changes the window | pass |
| run defaults persisted | pass |
| evidence settings persisted | pass |
| agent settings persisted (caps and standing instructions) | pass |
| storage rules persisted | pass |
| shell preference persisted | pass |
| the appearance setting persisted in the window | pass |
| with format html the saved evidence is a self-contained page naming the run and its workflow hash | pass |
| and with lineage off it points to the JSON form instead of embedding events | pass |
| with format json it holds the run, the artifacts with digests and the lineage, and a document hash | pass |
| the plan counts what each rule would remove and keeps the newest output of every partition | pass |
| without the keep rule the plan is larger and lists real artifacts | pass |
| the cleanup removed the artifact bytes from disk | pass |
| and the removed artifacts no longer list | pass |
| the runs themselves stay (history is not deleted) | pass |

Screenshots: 09-appearance.png, 09-storage.png

### s10: 13 of 13 checks passed

| Check | Result |
|---|---|
| the engine reports the local runtime with its isolation kind | pass |
| and says the sample suite is scripted agents, not models | pass |
| the run finished | pass |
| eight runs: four tasks for each of two agents | pass |
| the reference script passed every task, including the network task | pass |
| the idle script passed none | pass |
| every run says how it was isolated: an AppContainer with the network denied | pass |
| the engine opened a loopback listener for the network task and nothing connected to it | pass |
| control: outside the sandbox the same script reaches the listener and answers "reachable" | pass |
| a trajectory verifies as a hash chain | pass |
| the benchmark added no run and no workflow to the app | pass |
| its files are outside the engine state directory | pass |
| and a benchmark pointed at a state directory inside the engine state is refused | pass |

Screenshots: 10-benchmarks-after.png, 10-benchmarks-before.png

