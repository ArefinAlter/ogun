#!/bin/sh
# Collects the pre-alpha release assets of one platform into OUT with release names.
#   sh package.sh windows-x64|linux-x64 OUT
# Paths can be overridden: BIN_DIR (ogun, ogund), STUDIO_DIR (the Tauri target/release directory).
set -eu
PLATFORM=$1
OUT=$2
V=0.0.1a
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
BIN_DIR=${BIN_DIR:-$ROOT/target/release}
STUDIO_DIR=${STUDIO_DIR:-$ROOT/apps/studio/src-tauri/target/release}
mkdir -p "$OUT"
STAGE=$(mktemp -d)
cp "$ROOT/LICENSE" "$ROOT/NOTICE" "$STAGE/"
cat > "$STAGE/README.txt" <<TXT
Ogun $V (pre-alpha), by Verne Ltd.

ogun   the command-line client (validate, hash, put, run, backfill, approvals, mcp, lsp, ...)
ogund  the daemon: one local listener, the run store, the scheduler and the core operators

Start the daemon with "ogund", then "ogun daemon status". The binaries are not signed.
Documentation: https://github.com/ArefinAlter/ogun (README, docs/IMPLEMENTATION_STATUS.md)
TXT
case "$PLATFORM" in
  windows-x64)
    cp "$BIN_DIR/ogun.exe" "$BIN_DIR/ogund.exe" "$STAGE/"
    (cd "$STAGE" && powershell.exe -NoProfile -Command "Compress-Archive -Force -Path ogun.exe,ogund.exe,LICENSE,NOTICE,README.txt -DestinationPath ogun-$V-windows-x64.zip")
    mv "$STAGE/ogun-$V-windows-x64.zip" "$OUT/"
    cp "$STUDIO_DIR"/bundle/nsis/*.exe "$OUT/ogun-studio-$V-windows-x64-setup.exe"
    P=$(mktemp -d)
    cp "$STUDIO_DIR/ogun-studio.exe" "$P/ogun-studio.exe"
    cp "$ROOT/LICENSE" "$ROOT/NOTICE" "$P/"
    (cd "$P" && powershell.exe -NoProfile -Command "Compress-Archive -Force -Path ogun-studio.exe,LICENSE,NOTICE -DestinationPath ogun-studio-$V-windows-x64-portable.zip")
    mv "$P/ogun-studio-$V-windows-x64-portable.zip" "$OUT/"
    ;;
  linux-x64)
    mkdir -p "$STAGE/ogun-$V-linux-x64"
    cp "$BIN_DIR/ogun" "$BIN_DIR/ogund" "$STAGE/LICENSE" "$STAGE/NOTICE" "$STAGE/README.txt" "$STAGE/ogun-$V-linux-x64/"
    tar -C "$STAGE" -czf "$OUT/ogun-$V-linux-x64.tar.gz" "ogun-$V-linux-x64"
    cp "$STUDIO_DIR"/bundle/appimage/*.AppImage "$OUT/ogun-studio-$V-linux-x64.AppImage"
    cp "$STUDIO_DIR"/bundle/deb/*.deb "$OUT/ogun-studio-$V-linux-x64.deb"
    ;;
  *) echo "unknown platform $PLATFORM" >&2; exit 2 ;;
esac
rm -rf "$STAGE"
ls -la "$OUT"
