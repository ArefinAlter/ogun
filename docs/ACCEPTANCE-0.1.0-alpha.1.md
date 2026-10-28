# Ogun 0.1.0-alpha.1 acceptance

This alpha includes the 0.0.1c desktop/workflow subscription transports, the merged responsive panel fixes, and the restricted Grok Build ACP adapter. Grok uses a separate official login in a dedicated profile; see [setup and protocol evidence](acceptance/grok-subscription.md). It does not scrape credentials or treat a consumer subscription as an API key.

Build source: `release/0.1.0-alpha.1`, based on main `ee42f2fe616a0169bbf50bcb55c82f9e3d1efb2e`. Shared core: `a944e43035e9de9f5871ec6afd10c036e13457ad`, with adaptive acquisition and the reviewed filesystem additions. Windows targets use native Rust/Cargo with a shared build lock, four build jobs, locked dependency resolution, and all cache/temp files on D. Each GUI acceptance run uses a fresh profile.

Completed checks are recorded below as they finish. This document is not a claim that unlisted checks passed.

- Shared Grok protocol/profile fixtures passed in the Dynodoc Windows integration (255 tests total, zero failures, three unrelated ignored checks) with strict Clippy. Ogun-specific native gates are recorded separately below.
- Ogun installed-agent transport: 12 Windows tests passed, zero failures, one subprocess fixture helper ignored. Includes the final Grok auth/config separation and ACP denial/limit fixtures.
- Native Rust workspace: strict Clippy passed; 359 tests passed with 5 explicitly ignored checks (fixture subprocess helper, two external-network acquisition/crawl checks, live Claude subscription and external Podman coverage). No test failed.
- Final installed-agent transport source passed a focused strict Clippy check after executable discovery ordering was finalized.
- Studio native shell: strict Clippy passed; all 7 tests passed.
- Frontend: all 58 tests passed; TypeScript and production asset build passed.
- Windows Edge interaction acceptance passed at 1366×768, 1024×600, 960×500, 800×600, 640×480, 390×500, 1280×400 and 1920×1080. Root scrolling remained inside the viewport. Thread details opened, closed from the pane and reopened at narrow widths. Composer and workspace remained visible. No page errors were recorded.

External limits: the new Grok adapter is source-audited and fixture-tested separately from live account acceptance; no live Grok request is claimed. Codex, Cursor and Antigravity live entitlement has not been newly verified in this alpha. Existing 0.0.1c Claude evidence remains historical evidence, not a new alpha live test. Windows binaries are unsigned. No new Linux, macOS, Android or iOS binary acceptance is claimed here.
