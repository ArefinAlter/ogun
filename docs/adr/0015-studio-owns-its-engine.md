# ADR 0015: Studio is standalone: it owns its engine

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-26 |
| Decided by | Pending the maintainer. Built on the maintainer's instruction of 2026-10-26 to make the desktop applications "actual softwares", one installer with no daemon to start. |
| Refines | ADR 0009 (Studio is a Tauri 2 application) and ADR 0010 (the server is optional, not the way in) |
| Constrained by | ADR 0002 (embedded engine, no browser sessions), Zeus ADR 0007 (human-only approvals), Zeus ADR 0010 (one listener) |
| Amends | ADR 0013 decision 3, last sentence ("Studio never asks for, shows or stores" a secret value), for the desktop shell only; see decision 10 |
| Specifies | O-15 sections 2, 4 and 6; O-12 |

## Context

Until release 0.0.1a, Studio's desktop shell was a window onto a daemon somebody else had started, or onto a hosted
hub through a read-only API token. A person who installed Studio saw an error page until they had installed the
`ogun` command-line tool, started `ogund` and created a `studio` credential by hand. The maintainer asked for software:
install, open, work, with no command line.

Three of Ogun's own rules decide how that can be done:

- Approvals are human-only (constraint 3). The daemon recognises a person's decision by the program on the other end
  of its endpoint, and until now it could do that only on Linux. On Windows no connection was interactive, so a
  Windows person could not decide an approval at all.
- A secret stays out of files other programs can read and out of the webview (constraint 4).
- `ogund` has exactly one listener, a local IPC endpoint (Zeus ADR 0010 pattern, O-15 section 2).

## Decision

1. **The engine is a sidecar the shell starts.** Studio ships `ogund` next to `ogun-studio` (Tauri `externalBin`),
   together with the programs the engine and the shell start: `ogun` (the CLI, also a trust surface), `ogun-ingest`,
   `ogun-op-llm`, `ogun-op-shim`, `ogun-bench`, and on Windows `ogun-sandbox` (ADR 0017) and an embeddable Python.
   On launch the shell starts `ogund --supervised --profile studio`. There is no in-process engine: a crash of the
   engine must not take the window with it, operators are child processes anyway, and the daemon keeps its single
   listener.
2. **No TCP port.** The daemon listens on its local endpoint only, a named pipe on Windows and a Unix socket
   elsewhere (O-15 section 2). The Windows pipe is created with an access list that names only the current user's
   security identifier, rejects remote clients and must be the first instance of its name, so another user, another
   machine, or a program that squats the name cannot take part.
3. **A per-launch credential on stdin, never on disk.** At every start the shell makes a fresh `human` credential
   named `studio` and writes one line (`<id> human studio <token>`) to the daemon's standard input. The daemon keeps
   it in memory only (`--supervised`, `StartOptions.transient_credential`); there is no token file for another program
   to read, and a restart replaces it. The O-15 handshake still runs: the token itself never crosses the endpoint.
   A supervised daemon also provisions a persistent `service` credential named `ingest` for the ingest bridge, which the
   shell starts beside it and restarts if it exits.
4. **The shell supervises.** The daemon prints one `{"event":"ready",...}` line when it listens. If it exits, the shell
   starts it again with a growing delay (one second, doubling, at most sixteen); a run of 60 seconds counts as good and resets the
   count; after six failed starts in a row the shell stops trying, says why and names the log
   (`<state>/logs/ogund.log`, rotated at 5 MiB), and waits for the person to press Restart. Stopping is closing the
   daemon's stdin: it checkpoints in-flight attempts and exits; after ten seconds the shell kills it. If the shell is
   itself killed, the operating system closes the pipe and the daemon stops the same way, so no engine outlives the
   program that holds its credential. A daemon started by hand can still be used for development
   (`OGUN_STUDIO_ENGINE=external`).
5. **Its own profile and data.** The engine runs as profile `studio`, so its state is
   `%LOCALAPPDATA%\Ogun\profiles\studio` (and the platform equivalents), separate from a default `ogund` a person
   may run themselves (O-15 section 2). The shell keeps its own small files (preferences) in the application data
   folder (`OGUN_STUDIO_DATA` overrides it for tests and portable installs). Uninstalling removes the program and
   leaves both folders. Upgrades install new binaries; the daemon opens its own state and migrates it where a format
   changes, and Studio has no migration step.
6. **One instance, optionally resident.** A second launch brings the first window forward. Closing the window stops
   the engine, unless the person turned on "keep running in the tray", in which case the window hides, the engine and
   its schedules keep running, and the tray menu offers "Quit and stop the engine".
7. **"This computer" is the default environment; a hub is optional.** The first window shows the local engine. The
   person may add a hub (an `ogun-server`, ADR 0010): with a read-only API token, or by signing in in a browser
   (O-13 section 4.3). Nothing in the first launch needs a network or an account. On the first launch Studio offers to
   import one of two sample projects (`examples/starter`, `examples/bikeshare`) through `workflows.put`, which is what
   `ogun put` does.
8. **Studio is a trust surface, and Windows peers are inspected.** The daemon pins the `ogun` and `ogun-studio` files
   next to itself (canonicalised), plus any listed in `config.json` (`trusted_binaries`, `trusted_studio_binaries`).
   At accept time on Windows it asks the system which process holds the client end of the pipe
   (`GetNamedPipeClientProcessId`) and which executable that process runs (`QueryFullProcessImageNameW`); the client
   cannot choose either answer. After a `human` credential authenticates, the daemon assigns a surface
   (`ogun_api::policy::Surface`):
   - `terminal` when the peer is a pinned `ogun` binary (on Linux also with a controlling terminal; Windows has no
     equivalent test, so there the pinned image path is the whole check, and the CLI's own prompt reads from
     `CONIN$`);
   - `studio` when the peer is the pinned `ogun-studio` binary;
   - `none` otherwise, and always `none` for an `agent`, `service` or `bridge` credential.

   The surface is returned by `daemon.authenticate` and filters the method list. `approvals.decide` with `terminal`
   or `studio` evidence is accepted only from the matching surface (O-15 section 6). Rows marked `studio` in the
   policy table (storing and deleting a secret, shutting the daemon down) are open to the Studio surface; the
   trust-changing rows (`credentials.*`, `approvers.confirm` and `revoke`, `edges.enroll`) stay terminal-only.
9. **The webview is a closed client.** The window can name only the commands of the shell's closed lists
   (`commands.rs`, `local_cmds.rs`); it cannot read a credential, token or secret value, open a file or a process, or
   load remote content, and navigation away from the bundled pages is refused. `engine_call` relays only the daemon
   methods on a fixed list, none of which decides an approval, reads a secret value or runs code.

10. **A secret may be typed into the desktop window, and nowhere else.** ADR 0013 decision 3 says secret values are stored
    on a terminal and that Studio never asks for one. A person who has just installed Studio and wants to give it a model
    provider's key or an S3 secret has no terminal in front of them, so for the desktop shell that sentence is replaced
    by this rule: a masked field in Settings takes the value and sends it once, through the shell's `secret_put` command,
    to the engine's `secrets.put`, a row only the terminal and Studio surfaces may call (decision 8). The shell holds it in
    zeroizing memory for the call; the field is cleared when Save is pressed; the value is never returned, listed, logged,
    placed in an event, a lineage record, an error, a prompt, a URL or a process argument, and is never in the webview's
    local storage. Everywhere else a secret is referred to by its **name** (connections, providers, the agent), as ADR 0013
    says. Where the engine's credential store is unavailable the Settings page says where the value went (the operating
    system's credential store, a passphrase-encrypted file, or, last, an owner-only file). Studio-web and a hub link have
    no such field: a hub's secrets are the hub's.

## Consequences

- Install, launch and first workflow need no command line, no daemon start and no account. The installer is large
  because it carries the engine, its helpers and, on Windows, a Python.
- A Windows person can now decide approvals, in Studio with the facts-first screen, with `studio` evidence the daemon
  checks. Approvals remain human-only: nothing an agent, MCP client, API token or scheduler can reach decides one.
- **Honest limit, unchanged from O-15 section 4.4:** a program that runs as the same user and can start the pinned
  `ogun-studio.exe` or `ogun.exe` can present itself as that surface. The checks separate the surfaces from agents
  that run as other binaries or other credential kinds; they do not make a hostile same-user program harmless. The
  per-launch credential removes the token file from that picture, but not a process that can read another process's
  memory.
- macOS is not inspected (`LOCAL_PEERPID` is not implemented), so on macOS no connection is a trust surface and Studio
  could not decide an approval; macOS desktop is planned, not built.
- A person who also runs `ogund` themselves has two engines with two profiles. Studio does not adopt the other one.
- The engine stops with the window unless the person asks otherwise, so schedules fire only while Studio is open or
  resident. A system service would change that; it is not offered.

## Alternatives considered

- **Link `ogund` into the shell as a library:** rejected. A crash or a stuck attempt would freeze the window, the
  one-listener rule would move into the webview process, and operators still need child processes and a sandbox.
- **A loopback TCP port with a random secret:** rejected. A port can be scanned and, on a multi-user machine, reached by
  other accounts; a pipe whose access list names one user cannot.
- **A token file for the `studio` credential:** rejected. Any program of the same user could read it. Stdin leaves no
  file.
- **A Windows service or a login item that always runs the engine:** rejected for now. It needs administrator
  rights at install or a second installer step, and per-user data is simpler. The tray option covers "keep running".
- **Treat every Windows connection as interactive:** rejected. That would let an agent that can open the pipe decide an
  approval, which is exactly what constraint 3 forbids.
