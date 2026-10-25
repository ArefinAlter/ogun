"""Weekly ridership forecast for an Ogun `python` code operator (illustrative).

Inputs: daily trips per station (JSON Lines: day, station_id, trips) and the weather forecast for the city from
Open-Meteo (JSON). Output `rows`: the next 7 days, per station, expected trips.
Model: the same two weeks a year ago as the baseline, scaled by the forecast against last year's weather for that
week: cooler and wetter weeks carry fewer trips. Deliberately simple and explainable; swap it for a better model by
changing this file, which changes its hash and therefore needs a person's approval before it runs.
"""

import json
import sys
from collections import defaultdict
from datetime import date, timedelta


def main():
    start = json.loads(sys.stdin.readline())
    today = date.fromisoformat(start["config"]["as_of"])
    trips, temps, rain = defaultdict(int), [], []
    for item in start["inputs"]:
        if item["port"] == "trips":
            with open(item["path"], encoding="utf-8") as f:
                for line in f:
                    r = json.loads(line)
                    d = date.fromisoformat(r["day"])
                    if today - timedelta(days=371) <= d < today - timedelta(days=357):
                        trips[r["station_id"]] += r["trips"]
        elif item["port"] == "forecast":
            with open(item["path"], encoding="utf-8") as f:
                doc = json.load(f)
                temps = [v for v in doc["hourly"]["temperature_2m"] if v is not None]
                rain = [v for v in doc["hourly"]["precipitation"] if v is not None]
    last_year_temp = start["config"].get("last_year_week_temp_c", 14)
    temp = (sum(temps) / len(temps)) if temps else last_year_temp
    wet_hours = sum(1 for v in rain if v > 0.2)
    scale = max(0.4, min(1.6, 1 + (temp - last_year_temp) * 0.03 - wet_hours * 0.004))
    out = "ridership.jsonl"
    with open(out, "w", encoding="utf-8") as f:
        for station in sorted(trips):
            weekly = trips[station] / 2  # two weeks of history around the same week last year
            f.write(json.dumps({"station_id": station, "week_from": today.isoformat(), "trips": round(weekly * scale)}, sort_keys=True) + "\n")
    print(json.dumps({"type": "output", "port": "rows", "path": out, "media_type": "application/x-ndjson"}))
    print(json.dumps({"type": "result", "ok": True}))


if __name__ == "__main__":
    main()
