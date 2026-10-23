# O-12. Platform profiles

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-03, O-09, O-10 |
| Decided by | ADRs 0009 and 0010 |
| Implemented by | CI matrix (`.github/workflows/ci.yml`); Studio shells in `apps/studio`: the desktop and mobile Tauri shell (`ogun-studio`), the shared frontend served as Studio-web, the Android field gateway (`tauri-plugin-ogun-gateway`) |

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
| Code operators (Python, SQL, dbt, shell) | full | full | partial: container preferred | full | partial: no Landlock on old kernels | no | no |
| LLM operators | full | full | full | full | full | partial: remote providers only | partial: remote providers only, foreground |
| Stream ingest (MQTT, HTTP push, LoRaWAN, Modbus) | full | full | full | full | full | partial: MQTT client and BLE relay in the foreground service | no |
| Store-and-forward and edge-to-hub sync | full | full | full | full (hub side) | full | partial | no (pulls from the hub) |
| Benchmark profile (containers) | full | partial: Docker or WSL 2 | partial: Docker or Lima | full | no | no | no |
| Zeus session bridge | full | full | full | no (no browser) | no | no | no |
| Studio | full (Tauri) | full (Tauri) | full (Tauri) | full (Studio-web) | remote | partial: companion | partial: companion |
| Human approvals | full: CLI tty, Studio | full | full | partial: Studio-web with WebAuthn, after review (ADR 0010) | full: CLI tty | later: biometric channel evidence | later: biometric channel evidence |
| MCP server | full (stdio) | full (stdio) | full (stdio) | full (HTTP bridge) | full (stdio) | no | no |
| VS Code extension | full | full | full | remote (VS Code Remote) | remote (VS Code Remote-SSH) | no | no |

Edge-class devices also have a minimum window size (O-09) of 10 s. The hub minimum is 1 s.

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

A row in section 2 is a target until it has a line in this table showing it ran. The CI jobs in `.github/workflows/ci.yml` add lines here as they pass.

## 5. CI tiers

- **Tier A, required:** Linux x86_64, Linux aarch64, Windows x86_64.
- **Tier B, informational until an acceptance run passes:** Windows aarch64, macOS Intel, macOS Apple Silicon, and type-checks for Android aarch64 and iOS.
- **Tier C:** iOS companion; manual acceptance only.

## 6. Open issues

1. Sandbox backend per OS: Landlock and seccomp on Linux, AppContainer and job objects on Windows, and macOS, whose `sandbox-exec` is deprecated. This feeds O-03.
2. The WebAssembly target (`wasm32-unknown-unknown`, for Studio-web validation in the browser) needs the `getrandom` `wasm_js` backend for ULID generation, or an id-free build of `ogun-ir`.
3. Battery and data budgets for the Android gateway.
4. The Android gateway's ingest: the matrix lists the MQTT client and a BLE relay. `apps/studio/plugins/gateway` implements the MQTT client and also HTTP push from devices on the local network, which the matrix does not list yet; the BLE relay is not implemented. The matrix should either list LAN HTTP push or the gateway should drop it before release.
