# Acceptance run for Ogun 0.0.1b

What was run, how, what passed, and what is only partly verified. Written by the agent that built the release; the
evidence is the output of `tools/acceptance/` (scenario scripts, a fixture server, an identity-provider stand-in and a
hub stack), kept in `D:\verne-build\ogun\evidence\<scenario>\` on the build machine and summarised here. Nothing in this
file is a claim the evidence does not back; where something was checked against a stand-in rather than the real service, the
row says so.

## Method

- **The real app.** Each scenario starts the real `ogun-studio.exe` (the Tauri shell with its bundled `ogund` and the other
  sidecars) on a scratch profile (`OGUN_HOME`, `OGUN_STUDIO_DATA`, `WEBVIEW2_USER_DATA_FOLDER` under a scratch folder), so
  the owner's real app data is never read or written. The harness drives the app's window through the WebView2 remote
  debugging port (Playwright over CDP) and calls the engine through the app's own commands (webview, shell, named pipe,
  `ogund`), the path the screens use. It takes screenshots of the real window.
- **Real inputs.** A local fixture server (`fixture_server.py`, loopback, dual stack, `fixture.localtest.me`) stands in
  for HTTP sources, an S3-compatible store that checks AWS Signature V4, and an OpenAI-compatible model endpoint. The public
  GitHub REST API is used for real in scenario 2.
- **Stand-ins, named.** The identity provider in scenario 7 is `idp_mock.py`, which behaves like Dex for a public client
  (authorization code with PKCE S256, loopback redirect, refresh-token rotation, ES256 ID tokens). It is not Dex. The hub is a
  real `ogun-server` with a real `ogund`. The model provider in scenario 6 is a local server that speaks the OpenAI chat
  completions wire format. No model API key exists on the build machine, so **no live provider was called**.
- **One hook.** Scenario 9 makes runs look 40 days old with `OGUN_STORAGE_CLOCK_OFFSET_DAYS=40` so the retention rules have
  something to act on; the removal that follows is real (bytes leave the artifact store on disk).
- **Machine.** Windows 11 Pro, 16 logical CPUs shared with other builds, so timings in the evidence are slow and not a
  benchmark.

## Results

{{RESULTS}}
