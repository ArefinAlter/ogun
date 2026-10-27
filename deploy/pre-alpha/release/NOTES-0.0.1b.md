Ogun Studio is now a standalone desktop app: one installer, its own engine, no daemon to start, no account, and no hub or network needed to run it. This is a **pre-alpha release**, from Verne Ltd.

## What it is

Ogun is a local-first developer platform for data acquisition, data engineering and agent benchmarks. **Ogun Studio** is its desktop app. From 0.0.1b the app carries its engine (`ogund`) and starts it for you on a private pipe with a per-launch secret, restarts it if it dies, and stops it when you close the app. Your data stays in your own user folder (`%LOCALAPPDATA%\Ogun` on Windows, `~/.local/state/ogun` on Linux). A hub (a team server) is optional; the app works fully without one.

## New since 0.0.1a

- **Standalone Studio.** Install, start, work. The Local environment is the default; the first screen offers two sample projects that run on your computer. No separate daemon, no server, no account.
- **Real engine, real operators.** `acquire`, `parse`, `filter`, `partition`, `hash`, `manifest` and the rest of the core set run on your inputs, including live public APIs; backfills over hundreds of partitions survive killing the engine or the whole app and run only what did not finish.
- **Approvals in Studio.** Facts first, an arming delay, your decision bound to the request hash and the Studio channel (the daemon checks the peer process). Nothing a script, an agent or another program does can decide one.
- **Code operators in a Windows sandbox.** Python code (with the small Python 3.13 that ships in the installer) runs only after you approved its exact hash, inside an AppContainer with no network and no access to your files beyond its inputs and scratch folder. Details and limits below.
- **Agent threads with your own key.** An agent inside the engine calls the model provider you configure (Anthropic, OpenAI and compatible endpoints, Gemini, Ollama) with your key from the operating system's credential store, a closed tool table, per-thread access levels, cards you press, spend caps and an offline switch. Models stay optional.
- **Exporters.** Deliver results to a folder, a SQLite file or an S3-compatible bucket from a connection you set up (secrets by name), each delivery approved by you.
- **Sign in to a hub with your browser.** Authorization code with PKCE and a loopback redirect, tokens in the credential store, refresh, sign-out, the hub's roles. See "What does not work yet" for the hosted hub.
- **Benchmarks from the app.** A sample suite runs on your computer with the network denied and hash-chained trajectories. It checks the harness, not any model.

## Install

**Windows 10/11 x64:** run `ogun-studio-0.0.1b-windows-x64-setup.exe` (per-user, no administrator). Windows SmartScreen may warn because the file is not signed: choose "More info", then "Run anyway". Or unzip `ogun-studio-0.0.1b-windows-x64-portable.zip` anywhere and run `ogun-studio.exe`. Uninstalling removes the program and leaves the engine's data in `%LOCALAPPDATA%\Ogun` until you delete it.

**Linux x64 (glibc 2.35 or later):** `chmod +x ogun-studio-0.0.1b-linux-x64.AppImage` and run it, or `sudo apt install ./ogun-studio-0.0.1b-linux-x64.deb` on Debian or Ubuntu 22.04 or later.

`ogun`, `ogund` and `ogun-server` are also published as plain archives for each system.

## Windows code-operator sandbox: exactly what it does

Everything that starts another program (code operators, tool adapters, model helpers, local benchmark runs) is started by `ogun-sandbox.exe` in a Windows **AppContainer** inside a **job object**.

- It isolates: the network (not even this computer's loopback), your files (the program reads only its scratch folder, its inputs, its code file and its own program folder, and writes only in scratch), other processes and the desktop (at most 16 processes, UI restrictions, killed with the engine), memory and CPU time (per-process limits), and handles (only stdin, stdout and stderr are inherited).
- It does not isolate: the *names* of files in the folders that hold an operator's inputs and of sibling scratch folders; files Windows lets every app package read (most of the Windows and Program Files folders); which hosts a granted network reaches (all or nothing); disk space in scratch; and a flaw in Windows itself.
- A program folder installed for all users outside Program Files (for example `E:\Python310`) is copied once into the engine's state, up to 1.5 GiB; a bigger one needs a per-user install. `powershell.exe` does not start inside it.
- If the sandbox cannot start on your machine, operators that need it do not run at all; Settings says so and why. It is not a security boundary against a determined attacker with a Windows exploit. Details: `docs/windows-sandbox.md`.

On Linux the engine uses bubblewrap when it is installed (the AppImage does not bundle it). macOS is not part of this release.

## What does not work yet

- **The hosted pre-alpha hub still runs 0.0.1a.** Studio desktop signs in to a hub with your browser (authorization code with PKCE), but the hosted hub at `https://ogun.103.191.50.32.sslip.io` has to be upgraded first: its identity provider needs the public client `ogun-studio` and its `ogun-server` needs the 0.0.1b build. That upgrade was **not applied** (it needs the host's owner). Ready-to-run scripts are in `deploy/pre-alpha/native-signin/`. On the host: download `ogun-0.0.1b-linux-x64.tar.gz` from this release, then `./apply.sh <archive>` (backs up, edits `dex/config.yaml` and `server.json`, rebuilds the image, restarts `ogund`, `dex` and `ogun-server`), `./check.sh https://ogun.103.191.50.32.sslip.io`, and `./rollback.sh <backup folder>` to undo. Until then desktop sign-in to the hosted hub does not work; the app itself does not need it. The sign-in was verified end to end against a local `ogun-server` and `ogund` with an identity-provider stand-in that behaves like Dex for a public client, **not against Dex itself**.
- **No live model provider was called.** There was no provider key on the build machine. The four request formats (OpenAI-compatible, Anthropic Messages, Gemini, Ollama) were run against local servers written from each provider's public documentation. Mistral, DeepSeek, Qwen, Kimi and GLM use the OpenAI-compatible path and were not exercised separately.
- **Windows only was driven through the ten acceptance scenarios.** The Linux AppImage was started headless (Xvfb) from a scratch home: window, engine and ingest bridge started, and the engine stopped with the app; the `.deb` was inspected. No scenario was driven through the Linux build. macOS, Android and iOS are not built.
- **The binaries are not signed.** Windows SmartScreen warns on the installer.
- **Benchmarks in Studio run the sample suite only** (scripted agents, not models). The Harbor, Spider 2.0-DBT and ADE-bench adapters exist in the engine and are not offered in the app.
- **Streams:** one file-tail stream was run through the app. MQTT, LoRaWAN, Modbus and HTTP-push sources were not run in this acceptance.
- **Exports:** the S3-compatible delivery was verified against a local server that checks AWS Signature V4, not against S3 or MinIO. Every delivery waits for your approval.
- **Schedules run while the app is open.** Studio is not a service; nothing runs while it is closed.
- **Terminal approvals on Windows** are not available; approvals are decided in Studio. Remote (passkey) approvals stay off.
- **70 of 101 connectors are manifests only**; none is verified against its real service.

## Known issues

- `acquire` against `api.github.com` sometimes failed its first request with `http.protocol_error` from the shared transport. It is now treated as retryable, so runs recover on their own, but the cause is not understood.
- The `github-releases` sample reads the public GitHub API without a token, which allows 60 requests an hour per address; when that is used up the run fails with `http.status_403` and works again after the hour. (It happened during the acceptance run on this machine.)
- Open-Meteo, used by the earlier sample, disallows all crawlers in its `robots.txt`, and `acquire` obeys it; the starter project now reads the USGS earthquake catalogue instead.
- `powershell.exe` does not start inside the Windows sandbox, and a Python installed for all users outside Program Files is copied once into the engine's state (up to 1.5 GiB).
- Only one Studio per user at a time: a second launch does not start a second window or engine.

## Verification

The binaries were built from the code of commit `28e214b`; the commits after it change documentation and release text only. The acceptance run is in `docs/ACCEPTANCE-0.0.1b.md`: ten scenarios run on Windows 11 against the installed app, with a scratch profile each. All ten passed; scenarios 6 and 7 with limits, and every row says what was not covered.

| # | Scenario | Result |
|---|---|---|
| 1 | Install, launch, empty workspace, sample import | Passed (16 checks; also from the portable zip) |
| 2 | Core operators on real APIs, 121-partition backfill, kill `ogund`, kill the app | Passed (19 and 6 checks) |
| 3 | Schedules and a stream | Passed (14); file-tail source only |
| 4 | Approvals in Studio | Passed (15) |
| 5 | Code operators in the Windows sandbox | Passed (13) |
| 6 | Agent threads with your own provider key | Passed with limits (33): no live provider was called |
| 7 | Hub sign-in from the desktop app | Passed with limits (35): a local hub and an identity-provider stand-in, not Dex; the hosted hub was not upgraded |
| 8 | Exporters and connections | Passed (19); the S3-compatible server is local |
| 9 | Settings, evidence bundle, storage rules | Passed (16) |
| 10 | Benchmarks from the app | Passed (13); the sample suite only |

## Checksums

`SHA256SUMS.txt` is attached; the same lines:

```
ef64480815660bc89de1bd57d34a07363874bba71d2cf86d19d409f0627defdb  ogun-0.0.1b-linux-x64.tar.gz
ea1b6c9476047ccb326cc2bd4ced8e359663425b1958a7562b913ba0cdebce4e  ogun-0.0.1b-windows-x64.zip
e889efca70fb02c33f77cfb11e9b0f4bf5d58d02636bec6630ca3550bd8f5689  ogun-studio-0.0.1b-linux-x64.AppImage
74c17fc8d1d028215e9d85816cbe60d09f567c949fa3e730549145fbdff0cbd7  ogun-studio-0.0.1b-linux-x64.deb
0990f183c3afce709a286d99f13e5372e1ca66846f06fd4c99af1118c96af109  ogun-studio-0.0.1b-windows-x64-portable.zip
9cc3458d14d54b93be7549739fdd624603928763181e6a347caf858301dd347f  ogun-studio-0.0.1b-windows-x64-setup.exe
```

Apache-2.0. Verne Ltd., Dhaka, Bangladesh. Unsigned binaries.
