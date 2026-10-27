#!/bin/sh
# Collects the release assets of one platform into OUT with release names (0.0.1c).
#   sh package.sh windows-x64|linux-x64 OUT
# Paths can be overridden: BIN_DIR (the cargo release directory of the workspace), STUDIO_DIR (the Tauri target/release
# directory), PORTABLE_ZIP (the portable Studio zip made by apps/studio/scripts/portable.mjs). V sets the version.
set -eu
PLATFORM=$1
OUT=$2
V=${V:-0.0.1c}
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
BIN_DIR=${BIN_DIR:-$ROOT/target/release}
STUDIO_DIR=${STUDIO_DIR:-$ROOT/apps/studio/src-tauri/target/release}
mkdir -p "$OUT"
STAGE=$(mktemp -d)
cp "$ROOT/LICENSE" "$ROOT/NOTICE" "$STAGE/"
cat > "$STAGE/README.txt" <<TXT
Ogun $V (pre-alpha), by Verne Ltd.

ogun         the command-line client (validate, hash, put, run, backfill, approvals, mcp, lsp, ...)
ogund        the daemon: one local listener, the run store, the scheduler and the core operators
ogun-server  the team hub: HTTPS, sign-in with an OpenID Connect provider, Studio-web
ogun-ingest  the ingest bridge for stream sources
ogun-bench   the benchmark harness
ogun-op-llm, ogun-op-shim  helper programs the daemon starts for model operators and tool adapters
ogun-sandbox (Windows)     the helper that starts operators in an AppContainer

Start the daemon with "ogund", then "ogun daemon status". The binaries are not signed.
Documentation: https://github.com/ArefinAlter/ogun (README, docs/IMPLEMENTATION_STATUS.md, docs/ACCEPTANCE-0.0.1c.md)
TXT
case "$PLATFORM" in
  windows-x64)
    mkdir -p "$STAGE/pkg"
    for b in ogun ogund ogun-server ogun-ingest ogun-bench ogun-op-llm ogun-op-shim ogun-sandbox; do cp "$BIN_DIR/$b.exe" "$STAGE/pkg/"; done
    cp "$STAGE/LICENSE" "$STAGE/NOTICE" "$STAGE/README.txt" "$STAGE/pkg/"
    (cd "$STAGE/pkg" && powershell.exe -NoProfile -Command "Compress-Archive -Force -Path * -DestinationPath ../ogun-$V-windows-x64.zip")
    mv "$STAGE/ogun-$V-windows-x64.zip" "$OUT/"
    cp "$STUDIO_DIR"/bundle/nsis/*.exe "$OUT/ogun-studio-$V-windows-x64-setup.exe"
    cp "${PORTABLE_ZIP:?set PORTABLE_ZIP to the zip made by apps/studio/scripts/portable.mjs}" "$OUT/ogun-studio-$V-windows-x64-portable.zip"
    ;;
  linux-x64)
    mkdir -p "$STAGE/ogun-$V-linux-x64"
    for b in ogun ogund ogun-server ogun-ingest ogun-bench ogun-op-llm ogun-op-shim; do cp "$BIN_DIR/$b" "$STAGE/ogun-$V-linux-x64/"; done
    cp "$STAGE/LICENSE" "$STAGE/NOTICE" "$STAGE/README.txt" "$STAGE/ogun-$V-linux-x64/"
    tar -C "$STAGE" -czf "$OUT/ogun-$V-linux-x64.tar.gz" "ogun-$V-linux-x64"
    cp "$STUDIO_DIR"/bundle/appimage/*.AppImage "$OUT/ogun-studio-$V-linux-x64.AppImage"
    cp "$STUDIO_DIR"/bundle/deb/*.deb "$OUT/ogun-studio-$V-linux-x64.deb"
    ;;
  *) echo "unknown platform $PLATFORM" >&2; exit 2 ;;
esac
rm -rf "$STAGE"
ls -la "$OUT"
