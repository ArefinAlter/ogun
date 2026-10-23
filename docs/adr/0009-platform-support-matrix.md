# ADR 0009: Platform support: full runtime on desktop, server and edge; companion on mobile

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-23 |
| Decided by | Maintainer, 2026-10-23 ("accept for all") |
| Research | `docs/research/OR5-edge-streams-platforms.md` section 3 |
| Zeus counterpart | Zeus ADR 0009 (Tauri GUI), `zeus/docs/design/client-workflows-and-platforms.md` |

## Context

The maintainer asked for feature parity and device compatibility across Windows, Linux, macOS, Android and iOS, on desktop, mobile and a self-hosted server.

Identical features on every platform are not possible:

- **iOS** allows no child processes and no long-running daemon. App Review guideline 2.5.2 forbids running downloaded code.
- **Android** allows only binaries shipped inside the APK (since Android 10). Android 15 caps `dataSync` foreground services at 6 hours per day.

Ogun's design runs operators out of process and runs schedules in a daemon, so a promise of identical features everywhere would be false. A precise matrix, kept honest by CI, is worth more.

## Decision

1. **The matrix in O-12 section 2 is normative.** In short:
   - **Full runtime:** Windows, Linux and macOS (x86_64 and aarch64), the self-hosted server, and edge devices running Linux.
   - **Partial runtime on Android:** in-process core operators, streams and schedules only while a visible foreground service runs; no user code.
   - **Companion on iOS:** validation, viewing and (later) approvals against a desktop, server or hub.
2. **One core, many shells.** The pure crates compile for every target from the same source:
   - `ogun-types`, `ogun-ir`, `ogun-run`, `ogun-sched`, `ogun-stream`;
   - `ogun-store` and `ogun-edge`, which add redb.

   On 2026-10-23 these crates type-checked for `aarch64-linux-android` and `aarch64-apple-ios` with BLAKE3's pure-Rust backend. Native builds use the NDK or Xcode C toolchain instead.
3. **Tiers.**
   - **Tier A** (CI required): Linux x86_64 and aarch64, Windows x86_64.
   - **Tier B** (CI informational until an acceptance run passes): Windows aarch64, macOS Intel and Apple Silicon, an Android aarch64 type-check.
   - **Tier C** (companion only): iOS.
   - CI follows the Zeus layout (`zeus/.github/workflows/ci.yml`).
4. **Portable operators come later as WASI components.** WASI components are the only operator format that could run on all five systems, including iOS through an interpreter with bundled components only. Native processes remain the default where they are allowed.
5. **Studio is a Tauri 2 application in its own workspace** (the Zeus ADR 0009 pattern). The same frontend is served as Studio-web by the server (ADR 0010). On mobile, the companion is a Tauri 2 mobile shell. Background work on mobile uses the platform's own mechanisms, not Tauri's.

## Consequences

- Every feature claim names its platforms. Marketing copy must use the matrix, not "everywhere".
- Mobile is a companion and a field gateway: an Android phone can relay BLE sensor readings to an edge. That is a smaller promise than parity, and one Ogun can keep.
- CI cost grows. Tier B jobs stay informational until they have passed once on real hardware.

## Alternatives considered

- **Claim full parity:** rejected. iOS forbids it, and Android limits it.
- **A separate native app per platform:** rejected. That means three or four codebases for a small team.
- **No mobile at all:** rejected. It fails the requirement, and a companion that views runs and (later) approves is cheap given Studio-web.
