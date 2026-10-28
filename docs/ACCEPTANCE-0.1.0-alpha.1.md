# Ogun 0.1.0-alpha.1 acceptance

This alpha includes the 0.0.1c desktop/workflow subscription transports, the merged responsive panel fixes, and the restricted Grok Build ACP adapter. Grok uses a separate official login in a dedicated profile; see [setup and protocol evidence](acceptance/grok-subscription.md). It does not scrape credentials or treat a consumer subscription as an API key.

Executable/UI source: `8ec8a00a9db6baa7d2f7879ecc30a4a2ce45728d` on `release/0.1.0-alpha.1`, based on main `ee42f2fe616a0169bbf50bcb55c82f9e3d1efb2e`. Shared core: `a944e43035e9de9f5871ec6afd10c036e13457ad`, with adaptive acquisition and the reviewed filesystem additions. Windows targets use native Rust/Cargo with a shared build lock, four build jobs, locked dependency resolution, and all cache/temp files on D. Each GUI acceptance run uses a fresh profile.

Windows acceptance completed on 29 October 2026. The final release commit adds these acceptance receipts without changing executable/UI source.

- Shared Grok protocol/profile fixtures passed in the Dynodoc Windows integration (255 tests total, zero failures, three unrelated ignored checks) with strict Clippy. Ogun-specific native gates are recorded separately below.
- Ogun installed-agent transport: 12 Windows tests passed, zero failures, one subprocess fixture helper ignored. Includes the final Grok auth/config separation and ACP denial/limit fixtures.
- Native Rust workspace: strict Clippy passed; 359 tests passed with 5 explicitly ignored checks (fixture subprocess helper, two external-network acquisition/crawl checks, live Claude subscription and external Podman coverage). No test failed.
- Final installed-agent transport source passed a focused strict Clippy check after executable discovery ordering was finalized.
- Studio native shell: strict Clippy passed; all 7 tests passed.
- Frontend: all 58 tests passed; TypeScript and production asset build passed.
- Windows Edge interaction acceptance passed at 1366×768, 1024×600, 960×500, 800×600, 640×480, 390×500, 1280×400 and 1920×1080. Root scrolling remained inside the viewport. Thread details opened, closed from the pane and reopened at narrow widths. Composer and workspace remained visible. No page errors were recorded.

- Packaged Windows GUI: its bundled engine started with an isolated fresh profile and completed a real two-node offline `manifest` → `hash` workflow. Repeated native window resizing at 1280×720, 1024×620, 800×600, 640×560 and back to 1280×720 kept the workspace visible without document overflow. Thread details closed correctly; zero page errors. Compact and wide screenshots were reviewed.
- Packaged runtime archive: CLI, daemon, ingest, benchmark, hub server, edge sync and language server all reported `0.1.0-alpha.1`; offline workflow validation passed without a provider.
- NSIS installer build passed. The portable ZIP was checked byte-for-byte against the payload used in GUI acceptance, and both ZIPs passed CRC verification. Installer installation into an existing user profile is not claimed.

Release assets are Windows x64. `SHA256SUMS.txt` accompanies the installer, portable Studio ZIP and runtime ZIP at [the alpha release](https://github.com/ArefinAlter/ogun/releases/tag/v0.1.0-alpha.1).

External limits: the new Grok adapter is source-audited and fixture-tested separately from live account acceptance; no live Grok request is claimed. Codex, Cursor and Antigravity live entitlement has not been newly verified in this alpha. Existing 0.0.1c Claude evidence remains historical evidence, not a new alpha live test. Windows binaries are unsigned. No new Linux, macOS, Android or iOS binary acceptance is claimed here.
