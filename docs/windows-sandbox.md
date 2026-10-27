# The operator sandbox on Windows

What Ogun Studio does on Windows to keep the programs a workflow starts away from your files and your network, what you
will see about it, and where it stops. The decision record is [ADR 0017](adr/0017-windows-operator-sandbox.md); the
contract is [O-03](spec/O-03-operator-abi-v0.md) sections 3 and 4. This is pre-alpha software: read the limits.

## What runs inside it

A workflow is made of operators. Most of them are part of the engine (`acquire`, `parse`, `filter`, `partition`, `hash`,
`dedupe`, `export` and the rest of the core set): they are Ogun's own code and run inside the engine process, with the
engine's own rights, doing only what the workflow's closed configuration says. They are not sandboxed, and `export`
writes only where a connection you set up says.

Everything that starts **another program** runs inside the sandbox:

- **code operators**: `python`, `polars`, `sql`, `dbt`, `shell`, and only when you approved their code hash;
- **adapters**: `duckdb`, `pdf.extract`, `tesseract`, `ffmpeg`, `pandoc` and the other tools Ogun drives (each needs its
  program installed for your user; Studio does not install them);
- **model operators** (`llm.*`, `embedding.generate`), which are a small helper program that calls your model provider;
- **benchmark runs** that Studio starts on the "This computer, network denied" runtime.

If the sandbox is not available on your machine, those operators do not run at all. Ogun refuses them rather than run them
without it.

## What the sandbox does

Each program is started in a Windows **AppContainer** inside a **job object**, by a small helper, `ogun-sandbox.exe`, that is
installed next to the engine.

- **No network.** Not the internet and not this computer's own loopback addresses. A node whose manifest asks for
  network access gets outbound internet and nothing else; Windows offers no way to limit it to certain hosts.
- **Your files are out of reach.** The program can read its own scratch folder, the input files of that node, its code file
  and the folder its program lives in. It cannot read your Documents, Desktop, the rest of your profile, other programs'
  data or the engine's own state. It can write only in its scratch folder.
- **Limits.** A memory limit and a CPU-time limit per process, at most 16 processes, no clipboard or desktop access, and
  nothing is inherited from the engine except the program's standard input, output and error. When the engine stops, or you
  cancel the run, the whole tree of processes is ended.
- **Secrets.** A secret a node needs reaches the program on its standard input only, never in its command line, its
  environment or a file.

## Python

The installer carries a small Python 3.13 (the "embeddable" distribution from python.org, standard library only). A `python`
node finds it even on a computer with no Python. A node that needs packages needs a Python you install **for your own
user** (not "for all users" in a folder such as `E:\Python310`; see below). The Microsoft Store's `python3` shortcut is ignored
because it opens the Store instead of running Python.

## What you see

- **Settings, General, "Sandbox, egress and secrets", row "Code operators".** A green chip `sandboxed: appcontainer`, a
  sentence, an **Isolated** list and a **Not isolated** list, written by the engine. If the sandbox cannot start, the chip
  says **disabled on this machine** and the sentence says why. These words come from the engine itself
  (`sandbox.status`); Studio does not make them up.
- **About.** The same sentence under "code operators".
- **A workflow that needs a code operator on a machine without a sandbox** fails that node with
  `op.sandbox_unavailable`; nothing runs.
- **Benchmarks.** The runtime list says "This computer, network denied" is available only when the sandbox works; otherwise
  it offers Docker or Podman if either answers, and says so if none does.

## Limits, stated plainly

- An AppContainer is a rule that the Windows kernel enforces. It is **not** a virtual machine and **not** a boundary against
  a flaw in Windows itself. Treat it as strong protection against a careless or curious program, not against a determined
  attacker with an unpatched kernel exploit.
- **Names stay visible.** Windows lets a container open a file only if it can also list the folder that holds it. So the
  program can see the *names* of the files in the folders that hold its inputs, and the names of other nodes' scratch
  folders next to its own. It cannot open them.
- **Files Windows already shares with every app** (most of the Windows and Program Files folders) are readable by the
  program. Nothing in your own profile is among them.
- **Network access is all or nothing.** There is no per-host list.
- **No disk quota** on the scratch folder.
- **One profile for all operators.** The access entries Ogun adds name one Windows profile, `dev.verne.ogun.operator`, which
  no other program uses, and they stay in place after a run. Two operators therefore share rights to anything granted to either
  for as long as the entries stay; they are never granted outside the folders Ogun made for operators.
- **Programs installed "for all users" outside Program Files** (for example `E:\Python310`) cannot be opened to the sandbox by
  a per-user process. Ogun copies that folder once into its own state folder (up to 1.5 GiB) and runs the copy; a bigger
  tree needs to be installed for your user.
- **PowerShell did not start inside the sandbox** when the benchmark runtime tried it. A benchmark task that needs it needs
  Docker or Podman.
- **Model keys cannot be passed into a sandboxed benchmark run** (they would have to be written into the helper's plan
  file). A benchmark agent that needs a key must run as a host-placed agent or in a container.
- **Windows only, one machine.** This sandbox was written and tested on Windows 11 x64. It has not been run on Windows on
  Arm or older Windows 10 builds.

## Things left on your disk

Ogun adds an access entry for the sandbox profile to the folders it hands to operators (their scratch folders, the folder
of a program it runs, a benchmark's folders). They are harmless to every other program and are not removed when you
uninstall. Removing them is manual (for example with `icacls`), or you can delete the folders: Ogun's state folder is
`%LOCALAPPDATA%\Ogun\profiles\studio` and benchmark runs live under `%LOCALAPPDATA%\OgunBench`.

## For developers

- The helper is `crates/ogun-sandbox` (a thin binary) over `crates/ogun-platform` (`sandbox.rs`, the only place with
  `unsafe` besides the named-pipe module). `ogun-sandbox --self-test` starts `cmd /c exit 7` inside the sandbox and checks the exit
  code; the engine runs it once before it trusts the helper.
- The plan is a JSON file: `scratch`, `read_write[]`, `read_only[]`, `network`, `memory_bytes`, `cpu_ms`, `env[]`, `cache_dir`.
  It holds paths and limits, never a secret, and the helper deletes it as soon as it has read it.
- The probes are in `crates/ogun-sandbox/tests/appcontainer.rs`. They run real programs (`cmd.exe`, `curl.exe`, a Python) and
  check what they could and could not do. Set `OGUN_SANDBOX_TEST_PYTHON` to run the Python probe against the embeddable
  distribution, and keep the tests' temporary folder small (they grant access entries).
