#!/usr/bin/env bash
# Adds Studio desktop sign-in to the hosted pre-alpha hub. Idempotent; backs up first; see README.md.
set -euo pipefail

ARCHIVE="${1:?usage: apply.sh /path/to/ogun-0.0.1b-linux-x64.tar.gz}"
HERE="$(cd "$(dirname "$0")" && pwd)"
STACK="$(cd "$HERE/.." && pwd)"            # deploy/pre-alpha
ROOT="${OGUN_PREALPHA_ROOT:-/opt/ogun-prealpha}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BACKUP="$ROOT/backups/native-signin-$STAMP"

[ -f "$ARCHIVE" ] || { echo "no such archive: $ARCHIVE" >&2; exit 1; }
[ -f "$STACK/dex/config.yaml" ] && [ -f "$STACK/server.json" ] || { echo "run this from the stack's native-signin folder" >&2; exit 1; }

mkdir -p "$BACKUP"
cp -p "$STACK/dex/config.yaml" "$BACKUP/dex-config.yaml"
cp -p "$STACK/server.json" "$BACKUP/server.json"
if docker image inspect ogun-prealpha:local >/dev/null 2>&1; then
  docker tag ogun-prealpha:local ogun-prealpha:0.0.1a
fi
echo "backup: $BACKUP"

python3 - "$STACK" <<'PY'
import json, pathlib, re, sys

stack = pathlib.Path(sys.argv[1])

# 1. Dex: a public client for the desktop app.
p = stack / "dex" / "config.yaml"
text = p.read_text()
if "id: ogun-studio" not in text:
    block = (
        "  - id: ogun-studio\n"
        "    name: Ogun Studio (desktop)\n"
        "    public: true\n"
        "    redirectURIs:\n"
        "      # A public client may redirect to a loopback address on any port (Dex checks the host, not the port).\n"
        "      - http://127.0.0.1/callback\n"
    )
    text = re.sub(
        r"(staticClients:\n(?:  .*\n|\n)*?)(staticPasswords:)",
        lambda m: m.group(1) + block + m.group(2),
        text,
        count=1,
    )
    p.write_text(text)
    print("dex: added the ogun-studio public client")
else:
    print("dex: the ogun-studio client is already there")

# 2. ogun-server: the provider names its native client.
p = stack / "server.json"
cfg = json.loads(p.read_text())
changed = False
for prov in cfg["oidc"]["providers"]:
    if prov.get("native_client_id") != "ogun-studio":
        prov["native_client_id"] = "ogun-studio"
        changed = True
if changed:
    p.write_text(json.dumps(cfg, indent=2) + "\n")
    print("server.json: native_client_id set")
else:
    print("server.json: already set")
PY

# 3. The 0.0.1b binaries into the prebuilt folder the image is built from, then the image.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
tar -xzf "$ARCHIVE" -C "$TMP"
BIN="$(find "$TMP" -name ogun-server -type f | head -1)"
[ -n "$BIN" ] || { echo "the archive has no ogun-server" >&2; exit 1; }
BINDIR="$(dirname "$BIN")"
mkdir -p "$ROOT/prebuilt/amd64"
for b in ogund ogun ogun-server ogun-ingest; do
  if [ -f "$BINDIR/$b" ]; then install -m 0755 "$BINDIR/$b" "$ROOT/prebuilt/amd64/$b"; fi
done
docker build -f "$STACK/../Dockerfile" --build-arg BINARIES=prebuilt -t ogun-prealpha:local "$ROOT"

# 4. Restart only what changed, in order.
cd "$STACK"
docker compose up -d ogund
docker compose up -d dex ogun-server
echo "applied. Check: ./check.sh https://ogun.103.191.50.32.sslip.io"
