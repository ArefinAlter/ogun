"""Filter-life estimate for an Ogun `python` code operator (O-03, protocol jsonl-stdio/0).

Illustrative model, to be calibrated with Smart Air's own test data before any customer sees a
number. It follows Smart Air's published method: a HEPA filter on high speed loses half its
effectiveness after about 1,400 hours in Beijing air, so loading scales with hours run and with the
particle mass the filter has seen.

Input (one artifact, JSON Lines): hourly rollups per purifier with
  device_id, hours_on (fixed point x100), speed (1..3), indoor_pm25_ugm3_x10, outdoor_pm25_ugm3_x10
Output (`rows`, JSON Lines): per purifier
  device_id, load_pct_x10, hours_left_estimate, due_by (ISO date) or null

No network, no clock: the reference time comes from the start message, so the same inputs always
give the same output (determinism class `pure`).
"""

import json
import sys
from datetime import date, timedelta

# Calibration constants. Replace with Smart Air's test curves; recorded in lineage via the code hash.
RATED_HOURS_HIGH = 1400          # hours on high to 50% effectiveness, Smart Air Sqair test
REFERENCE_PM25_UGM3_X10 = 850    # ambient PM2.5 assumed during the reference test (assumption)
SPEED_FACTOR_X100 = {1: 30, 2: 60, 3: 100}  # relative airflow by speed (assumption)
HOURS_PER_DAY_DEFAULT = 8


def load_increment_x1e6(hours_x100, speed, pm_x10):
    """Fraction of rated life used, in millionths, for one rollup row."""
    flow = SPEED_FACTOR_X100.get(speed, 100)
    # (hours_x100 / 100) * (flow / 100) * (pm / reference) / rated, in millionths, all integers.
    return (hours_x100 * flow * pm_x10 * 1_000_000) // (100 * 100 * REFERENCE_PM25_UGM3_X10 * RATED_HOURS_HIGH)


def main():
    start = json.loads(sys.stdin.readline())
    today = date.fromisoformat(start["config"]["as_of"])
    used = {}
    recent_hours = {}
    for item in start["inputs"]:
        with open(item["path"], encoding="utf-8") as f:
            for line in f:
                r = json.loads(line)
                d = r["device_id"]
                used[d] = used.get(d, 0) + load_increment_x1e6(r["hours_on"], r["speed"], r["indoor_pm25_ugm3_x10"])
                recent_hours[d] = recent_hours.get(d, 0) + r["hours_on"]
    out_path = "filter_life.jsonl"
    with open(out_path, "w", encoding="utf-8") as out:
        for d in sorted(used):
            load = min(used[d], 1_000_000)
            remaining_fraction = 1_000_000 - load
            hours_left = (remaining_fraction * RATED_HOURS_HIGH) // 1_000_000
            # The input covers the last 30 days; fall back to 8 h/day when a purifier has no history.
            per_day = max(recent_hours[d] // 100 // 30, 1) if recent_hours[d] else HOURS_PER_DAY_DEFAULT
            due = today + timedelta(days=hours_left // per_day) if hours_left else today
            row = {"device_id": d, "load_pct_x10": load // 1000, "hours_left_estimate": hours_left, "due_by": due.isoformat()}
            out.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
    print(json.dumps({"type": "output", "port": "rows", "path": out_path, "media_type": "application/x-ndjson"}))
    print(json.dumps({"type": "result", "ok": True}))


if __name__ == "__main__":
    main()
