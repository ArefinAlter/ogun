#!/bin/sh
# Stores the sample workflows and gives them a history: a backfill that runs for real, a failed forecast, a
# rebalance shortlist, and two versions that wait for a person's approval. Run from this directory.
set -eu
C="docker compose exec -T ogund ogun --profile server"
until $C workflows >/dev/null 2>&1; do sleep 2; done
cd_put() { docker compose exec -T -w /srv/seed ogund ogun --profile server put "$1"; }
for f in hub-heartbeat station-status trips-backfill ridership-forecast rebalance-plan district-report; do
  cd_put "$f.workflow.json"
done
$C backfill trips-backfill --from 2023-01 --to 2025-12 --yes
$C run rebalance-plan --wait || true
# The forecast source answers 503: the node retries with backoff and fails after its last attempt, so do not wait.
$C run ridership-forecast || true
$C run station-status --wait || true
# Version 2 of rebalance-plan changes code: it waits for an approval that only a person at this host can give.
cd_put rebalance-plan.v2.workflow.json
$C approvals || true
