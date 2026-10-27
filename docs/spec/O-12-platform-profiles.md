# O-12. Platform profiles

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-03, O-09, O-10 |
| Decided by | ADRs 0009 and 0010; ADR 0015 (Studio owns its engine) and ADR 0017 (the Windows operator sandbox) |
| Implemented by | CI matrix (`.github/workflows/ci.yml`); Studio shells in `apps/studio`: the desktop and mobile Tauri shell (`ogun-studio`), the shared frontend served as Studio-web, the Android field gateway (`tauri-plugin-ogun-gateway`); on desktop the shell supervises a bundled `ogund` (ADR 0015), and on Windows `ogun-sandbox` (`crates/ogun-sandbox`, `crates/ogun-platform`) |

## 1. Purpose

This spec states which Ogun features run on which platform. It is normative: product copy, the website and Studio must not claim more than this matrix. Each claim is backed by a CI job or by an acceptance run recorded in section 4.

## 2. Matrix

Legend:

- **full**: supported
- **partial**: limited, as noted
- **remote**: through a desktop, server or hub
- **no**: not possible, or not planned

| Feature | Linux x64/arm64 | Windows x64/arm64 | macOS Intel/Apple Silicon | Server (Linux container) | Edge (Linux arm64, e.g. Raspberry Pi) | Android arm64 | iOS/iPadOS |
|---|---|---|---|---|---|---|---|
| IR validate, hash, diff, plan | full | full | full | full | full | full (in-process) | full (in-process) |
| `ogund`: durable runs, schedules | full | full | full | full | full | partial: foreground service only; the OS may stop it | no |
| `acquire` (zeus-core engine) | full | full | full | full | full | partial: while the service runs | partial: foreground only |
| Core operators (hash, dedupe, archive, filter, window seal, rollup) | full | full | full | full | full | full | partial: foreground, user-started |
| Adapter operators (DuckDB, pdf, OCR, ffmpeg) out of process | full | full | partial (sandbox backend differs) | full | full | partial: binaries bundled in the APK | no |
| Code operators (Python, SQL, dbt, shell) | full | partial: AppContainer sandbox (ADR 0017); an embeddable Python 3.13 (standard library only) is bundled; any other tool must be installed for the current user | partial: container preferred | full | partial: no Landlock on old kernels | no | no |
| LLM operators | full | full | full | full | full | partial: remote providers only | partial: remote providers only, foreground |
| Stream ingest (MQTT, HTTP push, LoRaWAN, Modbus) | full | full | full | full | full | partial: MQTT client and BLE relay in the foreground service | no |
| Store-and-forward and edge-to-hub sync | full | full | full | full (hub side) | full | partial | no (pulls from the hub) |
| Benchmark profile (containers) | full | partial: Docker or WSL 2; the sample suite also runs on the local runtime in an AppContainer (O-06 section 14) | partial: Docker or Lima | full | no | no | no |
| Zeus session bridge | full | full | full | no (no browser) | no | no | no |
| Studio | full (Tauri) | full (Tauri) | partial: configured, not built; planned | full (Studio-web) | remote | partial: companion | partial: companion |
| Studio with its own engine (a bundled `ogund`, no server, no account) | partial: `.deb` and AppImage, smoke-tested only | full: NSIS installer and portable zip; acceptance run on Windows 11 x64 | no: planned | no (Studio-web is served by the hub) | no | no | no |
| Human approvals | full: CLI tty, Studio | full: CLI and Studio, by the pipe client's executable (O-15 section 4.4) | no: the daemon cannot yet inspect a peer there (O-15 section 4.4) | partial: Studio-web with WebAuthn, after review (ADR 0010) | full: CLI tty | later: biometric channel evidence | later: biometric channel evidence |
| MCP server | full (stdio) | full (stdio) | full (stdio) | full (HTTP bridge) | full (stdio) | no | no |
| VS Code extension | full | full | full | remote (VS Code Remote) | remote (VS Code Remote-SSH) | no | no |

Edge-class devices also have a minimum window size (O-09) of 10 s. The hub minimum is 1 s.

**Studio 0.0.1b packaging, and what it claims.** Windows x64: a per-user NSIS installer (no administrator rights) and a portable zip. Linux x64: a `.deb` and an AppImage, smoke-tested only (the package metadata and a headless launch), not run against a desktop session. macOS: the Tauri configuration exists; nothing was built, so macOS is planned, and on macOS the daemon cannot yet tell whether a peer is Studio (O-15 section 4.4), so no approval could be decided there. Windows arm64 and Linux arm64: not built. The agent threads (O-17), the connection editor and `export` (O-16, ADR 0016) and benchmark runs from Studio (O-06 section 14) are features of the engine, not of a platform; they were run on Windows only, against mock model providers and local fixture servers, and no real model provider was called. Studio's approvals on Windows use the same Studio evidence as on Linux; on Windows the daemon's check for the CLI is the pinned executable's path alone, because Windows has no controlling-terminal test (O-15 section 4.4).

## 3. What cannot run on mobile

- **iOS:** an always-on daemon, cron schedules, continuous streams, long backfills, any out-of-process operator, Python, dbt, shell, containers, benchmarks and the Zeus bridge.
- **Android:** user code, containers and benchmarks. Schedules and streams run only while a visible foreground service survives the OEM's battery policy. Android 15 caps `dataSync` foreground services at 6 hours per 24.

The mobile role is a **companion**: browse runs, artifacts and lineage from a desktop, server or hub; later, approve with biometric channel evidence. On Android it can also act as a **field gateway**, relaying BLE sensor readings to an edge.

## 4. Evidence

| Date | Target | What was shown |
|---|---|---|
| 2026-10-23 | x86_64-unknown-linux-gnu | Every crate builds and every test passes; MSRV 1.90 build. |
| 2026-10-23 | aarch64-linux-android, aarch64-apple-ios, aarch64-unknown-linux-gnu | `cargo check` of `ogun-types`, `ogun-ir`, `ogun-run`, `ogun-sched`, `ogun-stream`, `ogun-store` and `ogun-edge`, with BLAKE3's pure-Rust backend (no cross C toolchain was available). Not linked, not run on a device. |
| 2026-10-23 | x86_64-unknown-linux-gnu | Studio: `cargo check`, `clippy` and `cargo test` of the Studio workspace (`ogun-studio`, `tauri-plugin-ogun-gateway`); the frontend built and its Studio-web pages rendered in headless Chromium against a mock of the REST routes. The desktop shell was not launched against a daemon. |
| 2026-10-23 | aarch64-linux-android | `cargo check` and `clippy` of `tauri-plugin-ogun-gateway` (the gateway core, its JNI exports and Tauri for Android), with BLAKE3's pure-Rust backend. Not linked; the Kotlin service is not compiled; not run on a device. |
| 2026-10-26 | x86_64-pc-windows-msvc (Windows 11) | `ogun-sandbox`: seven probes with real programs pass (start and exit code; reads only what it is granted and writes only in scratch; the person's profile folder is closed; no network, loopback included; a CPU limit ends a busy loop; the tree dies with the helper; the embeddable Python reads its input, lists its scratch and finds the network and the profile closed). The sample benchmark suite ran through it with the network denied (`crates/ogun-bench/tests/sample_suite.rs`). The O1 exit tests pass on Windows: a 121-partition backfill and a stream window survive a killed daemon. Windows peer inspection (the pipe client's process id and image path) is tested in-process. |
| 2026-10-26 | Windows 11 x64, `ogun-studio` | The app started its own supervised `ogund`; a bundled sample was imported through `workflows.put`; a workflow that fetched a public API ran to artifacts and lineage, in the real window. The acceptance run (scenarios, evidence paths, limits) is `docs/ACCEPTANCE-0.0.1b.md`. |

A row in section 2 is a target until it has a line in this table showing it ran. The CI jobs in `.github/workflows/ci.yml` add lines here as they pass.

## 5. CI tiers

- **Tier A, required:** Linux x86_64, Linux aarch64, Windows x86_64.
- **Tier B, informational until an acceptance run passes:** Windows aarch64, macOS Intel, macOS Apple Silicon, and type-checks for Android aarch64 and iOS.
- **Tier C:** iOS companion; manual acceptance only.

## 6. Open issues

1. Sandbox backend per OS: Landlock and seccomp on Linux; macOS, whose `sandbox-exec` is deprecated. Windows has its AppContainer and job object backend (ADR 0017, O-03 section 4); it is not a boundary against a flaw in Windows and its limits are listed there. This feeds O-03.
2. The WebAssembly target (`wasm32-unknown-unknown`, for Studio-web validation in the browser) needs the `getrandom` `wasm_js` backend for ULID generation, or an id-free build of `ogun-ir`.
3. Battery and data budgets for the Android gateway.
4. The Android gateway's ingest: the matrix lists the MQTT client and a BLE relay. `apps/studio/plugins/gateway` implements the MQTT client and also HTTP push from devices on the local network, which the matrix does not list yet; the BLE relay is not implemented. The matrix should either list LAN HTTP push or the gateway should drop it before release.
