#!/usr/bin/env bash
# Undoes apply.sh from its backup folder.
set -euo pipefail
BACKUP="${1:?usage: rollback.sh /opt/ogun-prealpha/backups/native-signin-<time>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
STACK="$(cd "$HERE/.." && pwd)"
[ -f "$BACKUP/dex-config.yaml" ] && [ -f "$BACKUP/server.json" ] || { echo "not a backup folder: $BACKUP" >&2; exit 1; }
cp -p "$BACKUP/dex-config.yaml" "$STACK/dex/config.yaml"
cp -p "$BACKUP/server.json" "$STACK/server.json"
if docker image inspect ogun-prealpha:0.0.1a >/dev/null 2>&1; then
  docker tag ogun-prealpha:0.0.1a ogun-prealpha:local
fi
cd "$STACK"
docker compose up -d ogund
docker compose up -d dex ogun-server
echo "rolled back to the state in $BACKUP"
