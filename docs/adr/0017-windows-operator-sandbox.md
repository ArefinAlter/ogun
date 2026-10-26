# ADR 0017: Windows operator sandbox: an AppContainer in a job object

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-26 |
| Decided by | Pending the maintainer. Built on the maintainer's instruction of 2026-10-26; the product needs code operators and a local benchmark runtime on Windows. |
| Refines | ADR 0003 decision 3 (code runs sandboxed, out of process) and ADR 0005 (benchmark isolation) |
| Addresses | O-03 section 4 ("no sandbox on Windows in this version"); O-12 open issue 1 (the backend per OS) |
| Specifies | O-03 sections 3 and 4; O-06 section 3 |

## Context

On Linux and macOS a child operator runs in bubblewrap or `sandbox-exec`. On Windows there was no backend: child
operators (adapters, code, model operators) were refused unless a development flag turned the refusal off. A Windows
Studio that cannot run a `python` node, or run a benchmark with the network denied, is not the software the
maintainer asked for. Windows has two ways to confine a process without administrator rights and without a driver:
a job object (limits, kill on close) and an AppContainer (a low-box token that can open only what an access list names
for its profile, and no network unless a capability is granted). Containers (Docker, WSL 2) are not on a typical
machine and do not start in a second.

## Decision

1. **One helper, `ogun-sandbox`.** A small program (library `ogun-platform`, binary `ogun-sandbox`) installed beside
   `ogund` and `ogun-bench` reads a plan file, starts one program inside the sandbox, passes its standard streams
   through, waits, and returns its exit code. `ogund` detects it at first use and runs its self test (`cmd /c exit 7`
   inside the sandbox); if either fails the backend is `none` and child operators are refused as before. Exit code
   125 is the helper's own failure.
2. **What the helper builds.**
   - **An AppContainer** for the profile `dev.verne.ogun.operator`, created on first use. The process gets a low-box
     token: it can open only objects whose access list names the profile (or all application packages, which Windows
     does for most of the Windows and Program Files folders). Without a capability it has no network, loopback
     included. A manifest that grants `network: any` gets the `internetClient` capability: outbound internet, no host
     filtering.
   - **A job object**: killed when the helper dies (so the daemon killing the helper kills the tree), at most 16
     processes, a per-process memory limit and a per-process CPU-time limit from the manifest, die on unhandled
     exception, and UI restrictions (no desktop switching, display settings, exit-windows, global atoms, clipboard,
     system parameters, or handles to windows outside the job).
   - **An explicit handle list**: the child inherits its standard input, output and error and nothing else, so no handle
     of the daemon (the pipe, the store, a secret file) reaches it.
   - **A working-directory jail by access list**: the scratch directory is read-write for the profile; inputs, the code
     file and the program's own folder are read-only; a plan may also name further read-write folders (a benchmark's
     logs). Secrets reach an operator only on its standard input (O-03 section 4), never in the plan file, which holds
     paths, limits and non-secret environment and is deleted by the helper as soon as it has read it.
3. **Access rules learned by trying, kept as rules.** Windows behaves in ways the documentation does not state plainly;
   these are what the probes in `crates/ogun-sandbox/tests/appcontainer.rs` found, and the helper relies on each:
   - A container can open a file only if its access list names the profile **and** the container can list the folder
     that holds it. A grant on a file therefore also gives the profile a non-inherited, list-only entry on the parent
     folder: the folder's other files stay closed, but their names are visible.
   - The environment block of an AppContainer process must contain `LOCALAPPDATA`, or the process fails to start
     (error 203). The helper adds a folder of its own inside the scratch directory when the plan has none; it is never
     the person's real one.
   - `SetNamedSecurityInfo` walks every child of a folder, which is minutes for a large tree; a grant on a single object
     uses `SetFileSecurityW`, which changes that object only. Folder grants that must reach their children use the
     walking call once and are skipped on later runs when the entry is already there.
   - Most of the Windows and Program Files folders already carry an entry for all application packages and need no grant.
   - A program installed for all users **outside** those folders (for example `E:\Python310`) cannot be opened to the
     profile by a per-user process. The helper copies that program's folder once into the engine's own state
     (`<state>/runtime`) and opens the copy, but only up to 1.5 GiB; a larger tree (an interpreter with a data-science
     environment) needs a per-user installation.
   - The grants are added once and left in place; they name a profile that no other program runs as.
4. **A Python that always works.** The Windows installer carries the embeddable Python 3.13.9 distribution that
   python.org publishes (checked against a recorded SHA-256 when it is staged, PSF licence file inside) beside the
   application. It is small, in a folder the installer's user owns, and standard library only. The engine's tool lookup
   knows that folder, so a `python` node finds an interpreter on a machine that has none; an operator that needs
   packages needs a Python the person installs for their own user. The Microsoft Store `python3.exe` stub, which opens
   the Store instead of running Python, is skipped.
5. **Two users of the helper.** Code, adapter and model operators run through it (the daemon writes the plan beside the
   scratch folder). The local benchmark runtime (ADR 0005, O-06 section 3) runs every command of a run through it with
   the workspace as scratch, the logs read-write, the benchmark and tests folders read-only, no network, and a plan
   environment that carries a minimal `PATH`; because the plan file would carry them, model credentials cannot be passed
   into this runtime, and a run that lists `pass_env` is refused with `bench.isolation_unavailable` (a host-placed agent
   or a container runtime is the alternative).
6. **Studio says what it does.** `sandbox.status` returns the backend and two lists in plain words, what it isolates and
   what it does not (the next section is the same list, in longer form). Settings shows them as they are.

## What it does and does not do

It does: deny the network (an AppContainer without the capability cannot connect anywhere, loopback included; tested
against a listener the test opened, which saw no connection); keep a program out of the person's documents, profile and
the engine's state (the profile folder is not listable from inside; tested); make everything but scratch (and the
declared read-write folders) read-only or closed (tested for a granted input and a planted file); end a CPU loop at its
CPU limit; end the whole tree when the helper is killed (tested with a program that would otherwise finish); run `cmd.exe`,
`curl.exe` and the embeddable Python (tested with a Python script that read its input, listed its scratch folder, found the
profile folder closed and the network refused).

It does not: protect against a flaw in Windows (an AppContainer is a policy the kernel enforces, not a virtual machine; it is
**not** a boundary against a kernel vulnerability); hide the **names** of files in the folders that hold an operator's
inputs or its sibling attempt folders; hide files that Windows lets every application package read; filter hosts when
the network is granted (all or nothing); cap disk use (the scratch folder has no quota); separate two operators from each
other (they share one profile, and the access entries stay after a run); or constrain a program that needs something the
container withholds. **`powershell.exe` did not start inside it** when the local benchmark runtime first tried (the cause
was not investigated); a task that needs PowerShell needs a container runtime.

## Consequences

- Code and model operators, adapters and the local benchmark runtime run on Windows with an OS-enforced sandbox, and the
  `allow_unsandboxed` development escape is no longer the way to run `python` there.
- O-12's matrix can say "partial" for Windows code operators with reasons, instead of a "full" that had no sandbox
  behind it. Linux and macOS are unchanged.
- The daemon and the benchmark harness depend on a helper that must sit beside them; an installation without it falls
  back to refusing child operators.
- Access entries for the profile accumulate on folders operators were given. They are harmless to every other program, but
  removing them is manual (`icacls`), and the acceptance run records which folders it touched.
- The helper and the named-pipe module (ADR 0015) are the only Windows code that uses `unsafe`; both live in the
  isolated `ogun-platform` crate, and each block says why it is sound.

## Alternatives considered

- **Refuse child operators on Windows:** the previous state. It makes a Windows install unable to run code nodes.
- **A restricted token and a job object without an AppContainer:** can remove privileges and limit resources, but cannot
  deny the network or confine file reads without an access list for every file.
- **WSL 2 or Docker Desktop for every operator:** needs features and installs most machines do not have, and starts too
  slowly for small nodes. Containers stay the choice for benchmarks that ship images (ADR 0005).
- **Windows Sandbox or Hyper-V:** needs administrator rights and Windows Pro features, and starts a virtual machine per
  run.
- **A per-run AppContainer profile:** cleaner (entries could be removed with the profile) but creating and deleting
  profiles per run is slow and races when operators run in parallel; one shared profile with persistent entries was
  chosen, and its cost is stated above.
