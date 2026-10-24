# Ogun Studio

Ogun Studio shows the workflows, runs, partitions, approvals, artifacts, lineage, streams and edges of an Ogun
daemon (`ogund`) or hub (`ogun-server`). One frontend runs in several shells, as ADR 0009 and the O-12 matrix
describe:

| Shell | Platforms (O-12) | Talks to | Approvals |
|---|---|---|---|
| Desktop (Tauri 2) | Linux, Windows, macOS: full | the local `ogund`, with the `studio` credential (O-15) | yes, with `studio` evidence (see below) |
| Studio-web | Server (Linux container): full | `ogun-server`'s REST routes, with the session cookie | with a passkey, only when the server enables remote approvals (ADR 0010) |
| Android (Tauri 2 mobile) | partial: companion, plus the field gateway | a hub's REST routes, read-only, with an API token | later (O-12) |
| iOS and iPadOS (Tauri 2 mobile) | partial: companion | a hub's REST routes, read-only, with an API token | later (O-12) |

A desktop Studio can also connect to a hub read-only (Settings, "Connect to a hub"); that is how an edge is viewed
remotely.

## Layout

| Path | What |
|---|---|
| `web/` | The shared frontend: TypeScript, Preact, Vite. One data interface with two backends: `tauri` (the shell's command set) and `http` (Studio-web's REST routes, cookie session, `x-ogun-csrf` header, Server-Sent Events, WebAuthn). |
| `src-tauri/` | The `ogun-studio` crate and binary: the Tauri 2 shell for desktop and mobile. Its own Cargo workspace, excluded from the root workspace. |
| `plugins/gateway/` | `tauri-plugin-ogun-gateway`: the Android field gateway. Rust core on `ogun-edge` (ingest log, sealed segments, signed sync offers), a JNI shim, and the Kotlin foreground service in `android/`. |

## Frontend

```sh
cd apps/studio/web
npm ci
npm run build        # type-checks, then writes dist/
npm run dev          # http://localhost:5173 (for Studio-web work: OGUN_SERVER=https://host:port npm run dev)
```

`dist/` is what every shell loads: the Tauri shells embed it, and `ogun-server` serves it as Studio-web. Asset URLs
are relative and the app routes by URL hash, so it works under any mount point and needs no server rewrites. It
uses no inline script or style, so it runs under a strict CSP (`script-src 'self'; style-src 'self'`). Fonts (IBM
Plex Sans and Mono, EB Garamond; SIL OFL 1.1, licences in `public/licenses/`) are bundled; nothing is fetched from
a third party.

## Desktop

Prerequisites: Rust 1.90 or newer, Node 22, and the [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/)
for the OS (Linux: `libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libgtk-3-dev librsvg2-dev`;
Windows: WebView2 and MSVC; macOS: Xcode command line tools). Keep `zeus-core` beside this repository.

```sh
npm --prefix apps/studio/web ci && npm --prefix apps/studio/web run build
cargo run --manifest-path apps/studio/src-tauri/Cargo.toml
# or, with the Tauri CLI (cargo install tauri-cli --version '^2'), from apps/studio/src-tauri:
cargo tauri dev
```

Studio connects to the daemon of the current user (`OGUN_HOME` and `OGUN_PROFILE` select another state directory
or profile) with the credential file `credentials/studio.token`, which must be a `human` credential. If it is
missing, Studio says so; create it on that machine with the CLI (`credentials.add`, name `studio`, kind `human`).

Approvals: Studio shows only the facts the daemon computed, in a fixed order, with the request hash, and keeps an
agent's words in a separate block marked as not verified. A decision takes two steps: the Rust process reads the
approval fresh and records the decision and its request hash; the confirmation button names the decision and arms
after 1.5 s; confirming re-checks the hash and sends `approvals.decide` with `studio` evidence. The webview never
supplies the hash and nothing decides automatically. The daemon accepts `studio` evidence only from a `human`
connection whose peer executable is the pinned `ogun-studio` binary: put the built `ogun-studio` next to `ogund`,
or list its path under `trusted_binaries` in the daemon's `config.json` (O-15 section 4.4). In this draft of O-15
only Linux daemons inspect peers, so on macOS and Windows a decision from Studio is refused until the daemon gains
those platform modules; decide in the CLI there.

Lockdown: the capability grants the main window exactly the commands in `src-tauri/src/commands.rs` (each maps to
one daemon method with typed parameters); no shell, filesystem, HTTP, opener or dialog plugin is installed; the CSP
allows only the bundled assets and Tauri's IPC; the window cannot navigate away from the bundled pages. The daemon
credential and any hub token stay in the Rust process.

## Studio-web

`ogun-server` serves `web/dist` and the REST routes under `/api/v1` (the Studio-web REST contract). The frontend
signs nothing itself: the session cookie comes from the server's OIDC sign-in, every non-GET request carries the
`x-ogun-csrf` header (from `GET /api/v1/me`'s `csrf_token`, or the `ogun_csrf` cookie), live updates use
`GET /api/v1/events` (`event: ogun` frames), and an approver decides with a passkey: the frontend asks the server for
WebAuthn request options bound to the approval's request hash, calls `navigator.credentials.get`, and posts the
assertion; the daemon verifies it (O-15 section 7). The decide buttons appear only for approvers when the server
reports remote approvals enabled.

## Android

Prerequisites: the Android SDK and NDK, Java 17, `rustup target add aarch64-linux-android`, the Tauri CLI. From
`apps/studio/src-tauri`:

```sh
cargo tauri android init     # generates gen/android (not committed) and links the gateway plugin's library
cargo tauri android dev      # or: cargo tauri android build --apk
```

The Android app is a companion: Settings, "Connect to a hub", with the hub URL, an API token (it acts as an agent,
so it can read but never decide) and, for a hub with a private CA, the CA certificate. The token is stored in the
app's private data directory and never returned to the webview.

### Field gateway

The Gateway page turns the phone into a field gateway (O-12 section 3; O-10; O-11):

- **Collect.** A visible foreground service (`dataSync`) subscribes to an MQTT broker on the local network (MQTT
  3.1.1, QoS 1, persistent session, `mqtts://` for any password) and, when enabled, accepts HTTP push from devices on
  the local network: `POST /ingest` on the configured port, `application/json` (an object or an array of objects)
  or `application/x-ndjson`, at most 1 MiB, optionally with `Authorization: Bearer <device token>`. Only private,
  link-local and loopback peers are served.
- **Commit, then acknowledge.** Messages are batched (200 ms or 1 000 records) into the durable ingest log
  (`ogun-edge`, redb, immediate durability). The PUBACK and the `202` are sent only after the commit; under `block` a
  full log stops acknowledging, so the broker keeps the messages and HTTP push gets `503` with `Retry-After`.
  Payloads that are not JSON objects are counted as invalid and dropped; their bytes are never logged.
- **Seal.** Every 30 s the log is cut into segments on the phone's receive clock (5 minutes by default), each an
  NDJSON blob named by its BLAKE3 digest, offered as asset `gateway/<name>`, partition `w<UTC start>-s<first seq>`.
- **Sync.** A JobScheduler job runs on an unmetered network, and only while charging unless that is turned off. It
  sends the Ed25519-signed `sync.offer` (signature over the JCS form of the offer) to the hub's
  `/api/v1/sync/offer`, uploads the wanted chunks (512 KiB) to `/api/v1/sync/chunk`, and offers again to collect
  acknowledgements. Only acknowledged segments are deleted, and only then are their records released from the log.
- **Enrol.** The gateway creates its key pair on first use and shows the public key and fingerprint. A person enrols
  the key on the hub host (`edges.enroll`, on its terminal), compares fingerprints, and enters the returned `edg_` id
  on the Gateway page. Nothing is offered before that.
- **Secrets.** The broker password and the device token are encrypted with a key in the Android Keystore; the page
  learns only whether each is set. The gateway directory (log, key, segments) is excluded from backups.

Limits, as O-12 states them: Android may stop the service; Doze defers network access while the phone is idle (the
broker queues QoS 1 messages meanwhile); Android 15 limits `dataSync` foreground services to 6 hours per 24, and the
service stops itself when told the limit is reached. The gateway runs no user code and no daemon schedules. O-12
lists the Android gateway's ingest as "MQTT client and BLE relay"; the BLE relay is not built, and LAN HTTP push is
built but not yet in the matrix (O-12 section 6).

## iOS and iPadOS

On macOS with Xcode and `rustup target add aarch64-apple-ios`, from `apps/studio/src-tauri`:

```sh
cargo tauri ios init
cargo tauri ios dev
```

The iOS app is a read-only companion of a hub over its REST routes, in the foreground only (O-12). It has no
background ingest, no local daemon and, for now, no approvals.

## Verification so far

- `npm run build` (TypeScript check and Vite build) passes.
- `cargo check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` of this workspace pass on Linux
  x86_64 (the system webview libraries installed); the tests cover the gateway core (append, seal, signed offers,
  chunking, acknowledgement and release, reopen), the hub URL rules and the navigation guard.
- `cargo check` and `clippy` of `tauri-plugin-ogun-gateway` for `aarch64-linux-android` pass (with
  `CARGO_FEATURE_PURE=1` for BLAKE3, as no Android C toolchain was available); `ogun-studio` itself was not
  checked for Android, because `ring` needs the NDK's C compiler.
- Studio-web pages were rendered in headless Chromium against a mock of the REST routes, under a strict CSP, at
  desktop and phone widths, including the approval confirmation's arming delay.
- Not yet done: launching the desktop shell against a running `ogund`, any Android or iOS build (no SDK, NDK or
  Xcode in the build environment; the Kotlin sources are uncompiled), and device acceptance. O-12 section 4 records
  only what ran.
