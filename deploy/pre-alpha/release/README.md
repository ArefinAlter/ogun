# Building the pre-alpha release

Release 0.0.1a (tag `v0.0.1a`) was built by hand, without GitHub Actions:

| Assets | Built on | How |
|---|---|---|
| Windows x64: Studio installer and portable zip, `ogun` and `ogund` | Windows 11 | `cargo build --release --locked --bin ogun --bin ogund` at the root; `cargo tauri build --bundles nsis --config '{"bundle":{"active":true}}'` in `apps/studio/src-tauri` |
| Linux x64: Studio AppImage and .deb, `ogun` and `ogund` | Ubuntu 22.04 in Docker (`linux.Dockerfile`, glibc 2.35) | `build-linux.sh` inside the container, with the two checkouts mounted read-only at `/src/ogun` and `/src/zeus-core` |

Both Studio builds set `OGUN_STUDIO_DEFAULT_HUB_URL` and `OGUN_STUDIO_DEFAULT_HUB_TOKEN` (the hosted hub and its public
sample token, `deploy/pre-alpha/README.md`); without them Studio links to the local daemon. `package.sh` names the files
as the release lists them; `SHA256SUMS` is `sha256sum` over all of them. The binaries are not signed.

`.github/workflows/release-prealpha.yml` does the same on Actions when started by hand with an existing tag.
