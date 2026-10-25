#!/bin/sh
# The nightly reset (03:00 UTC, /etc/cron.d/ogun-prealpha): the daemon's runs, workflows and artifacts and the
# service's database go; accounts, the bridge credential and the API tokens stay. Then the sample is seeded again.
set -eu
cd "$(dirname "$0")"
# Everything stops: Dex shares ogun-server's network namespace and must start again after it.
docker compose stop
docker run --rm --network none -v ogun-prealpha_ogun-state:/s alpine:3.20 \
  sh -c 'cd /s/profiles/server && rm -rf runs.redb artifacts code scratch streams sync'
docker run --rm --network none -v ogun-prealpha_svc-data:/d alpine:3.20 sh -c 'rm -f /d/svc.db /d/svc.db-wal /d/svc.db-shm'
docker compose up -d
sh ./seed.sh
echo "reset done at $(date -u +%FT%TZ)"
