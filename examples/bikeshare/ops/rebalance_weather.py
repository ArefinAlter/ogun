"""Rebalancing risk for an Ogun `python` code operator (O-03, protocol jsonl-stdio/0).

Sample model, version 2, weather aware: for every docking station, how often has it been empty, and how often full, at each hour of the day
over the last thirty days, and which hour is the worst. A station that is empty or full at the same hour on many
days is a candidate for moving bikes.

Input (JSON Lines): hourly rollups per station with
  station_id, capacity, hour (0..23), bikes_min, bikes_max
Output (`rows`, JSON Lines): per station
  station_id, empty_risk_pct_x10, empty_worst_hour, full_risk_pct_x10, full_worst_hour

On a wet or cold day (config `wet_or_cold`) riders skip the evening peak, so the evening hours count for less.

No network, no clock: the reference date comes from the start message, so the same inputs always give the same
output (determinism class `pure`).
"""

import json
import sys
from datetime import date

EMPTY_AT_OR_BELOW = 1  # bikes at or below this count as an empty station
FULL_DOCKS_AT_OR_BELOW = 1  # free docks at or below this count as a full station


def weigh(hour, risk_x10, evening_weight_x100):
    """Evening hours (16 to 20) are scaled by the weather weight; every other hour counts in full."""
    return risk_x10 * evening_weight_x100 // 100 if 16 <= hour <= 20 else risk_x10


def main():
    start = json.loads(sys.stdin.readline())
    date.fromisoformat(start["config"]["as_of"])  # the reference date must be a date; the model uses no clock
    evening_weight_x100 = 60 if start["config"].get("wet_or_cold") else 100
    cells = {}
    for item in start["inputs"]:
        with open(item["path"], encoding="utf-8") as f:
            for line in f:
                r = json.loads(line)
                cell = cells.setdefault(r["station_id"], {}).setdefault(r["hour"], [0, 0, 0])
                cell[0] += 1
                if r["bikes_min"] <= EMPTY_AT_OR_BELOW:
                    cell[1] += 1
                if r["capacity"] - r["bikes_max"] <= FULL_DOCKS_AT_OR_BELOW:
                    cell[2] += 1
    out_path = "rebalance.jsonl"
    with open(out_path, "w", encoding="utf-8") as out:
        for station in sorted(cells):
            hours = cells[station]
            empty_hour = max(sorted(hours), key=lambda h: hours[h][1] * 1000 // hours[h][0])
            full_hour = max(sorted(hours), key=lambda h: hours[h][2] * 1000 // hours[h][0])
            row = {
                "station_id": station,
                "empty_risk_pct_x10": weigh(empty_hour, hours[empty_hour][1] * 1000 // hours[empty_hour][0], evening_weight_x100),
                "empty_worst_hour": empty_hour,
                "full_risk_pct_x10": weigh(full_hour, hours[full_hour][2] * 1000 // hours[full_hour][0], evening_weight_x100),
                "full_worst_hour": full_hour,
            }
            out.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
    print(json.dumps({"type": "output", "port": "rows", "path": out_path, "media_type": "application/x-ndjson"}))
    print(json.dumps({"type": "result", "ok": True}))


if __name__ == "__main__":
    main()
