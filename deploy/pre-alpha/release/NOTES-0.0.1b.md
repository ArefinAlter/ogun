Ogun Studio is now a standalone desktop app: one installer, its own engine, no daemon to start, no account, no network. This is a **pre-alpha release**, from Verne Ltd.

## What it is

Ogun is a local-first developer platform for data acquisition, data engineering and agent benchmarks. **Ogun Studio** is its desktop app. From 0.0.1b the app carries its engine (`ogund`) and starts it for you on a private pipe with a per-launch secret, restarts it if it dies, and stops it when you close the app. Your data stays in your own user folder. A hub (a team server) is optional; the app works fully without one.

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

**Windows 10/11 x64:** run `ogun-studio-0.0.1b-windows-x64-setup.exe` (per-user, no administrator). Windows SmartScreen may warn because the file is not signed: choose "More info", then "Run anyway". Or unzip `ogun-studio-0.0.1b-windows-x64-portable.zip` anywhere and run `ogun-studio.exe`. Uninstalling removes the program and leaves your data (`%LOCALAPPDATA%\Ogun`) unless you delete it.

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

{{NOT_YET}}

## Known issues

{{ISSUES}}

## Verification

The acceptance run for this release is in `docs/ACCEPTANCE-0.0.1b.md`: ten scenarios run on Windows 11 against the real app, which of them passed, with limits, and what was verified against local stand-ins rather than the real service.

## Checksums

`SHA256SUMS.txt` is attached; the same lines:

```
{{SUMS}}
```

Apache-2.0. Verne Ltd., Dhaka, Bangladesh. Unsigned binaries.
