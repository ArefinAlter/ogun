#!/bin/sh
# Inside ogun-rel-linux: copies the read-only checkouts to /work (without Windows build outputs), builds, and puts
# the assets in /out. CARGO_BUILD_JOBS defaults to 4 (a shared machine).
set -eu
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
mkdir -p /work/ogun /work/zeus-core /out
rsync -a --delete --exclude target --exclude node_modules --exclude dist --exclude 'dist-*' --exclude .design-reference --exclude gen /src/ogun/ /work/ogun/
rsync -a --delete --exclude target /src/zeus-core/ /work/zeus-core/
cd /work/ogun
CARGO_TARGET_DIR=/cache/target cargo build --release --locked --bin ogund --bin ogun --bin ogun-server --bin ogun-ingest
mkdir -p /out/bin
cp /cache/target/release/ogund /cache/target/release/ogun /cache/target/release/ogun-server /cache/target/release/ogun-ingest /out/bin/
if [ "${SKIP_STUDIO:-0}" != "1" ]; then
  cd /work/ogun/apps/studio/web
  npm ci --no-audit --no-fund
  cd /work/ogun/apps/studio/src-tauri
  CARGO_TARGET_DIR=/cache/studio-target cargo tauri build --bundles appimage,deb --config '{"bundle":{"active":true}}'
fi
BIN_DIR=/cache/target/release STUDIO_DIR=/cache/studio-target/release sh /work/ogun/deploy/pre-alpha/release/package.sh linux-x64 /out/assets || true
ls -la /out/bin /out/assets 2>/dev/null || true
