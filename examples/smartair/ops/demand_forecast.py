"""Weekly filter and purifier demand forecast for an Ogun `python` code operator (illustrative).

Inputs: daily shop sales (JSON Lines: day, sku, units) and the CAMS PM2.5 forecast for Dhaka from
Open-Meteo (JSON). Output `rows`: next 7 days, per SKU, expected units.
Model: same-week-last-year baseline, scaled by forecast PM2.5 against last year's level for the
same week. Deliberately simple and explainable; swap for a better model by changing this file,
which changes its hash and therefore needs a human approval before it runs.
"""

import json
import sys
from collections import defaultdict
from datetime import date, timedelta


def main():
    start = json.loads(sys.stdin.readline())
    today = date.fromisoformat(start["config"]["as_of"])
    sales, forecast = defaultdict(int), []
    for item in start["inputs"]:
        if item["port"] == "sales":
            with open(item["path"], encoding="utf-8") as f:
                for line in f:
                    r = json.loads(line)
                    d = date.fromisoformat(r["day"])
                    if today - timedelta(days=371) <= d < today - timedelta(days=357):
                        sales[r["sku"]] += r["units"]
        elif item["port"] == "forecast":
            with open(item["path"], encoding="utf-8") as f:
                doc = json.load(f)
                forecast = [v for v in doc["hourly"]["pm2_5"] if v is not None]
    last_year_pm = start["config"].get("last_year_week_pm25", 80)
    pm = (sum(forecast) / len(forecast)) if forecast else last_year_pm
    scale = max(0.5, min(3.0, pm / last_year_pm))
    out = "demand.jsonl"
    with open(out, "w", encoding="utf-8") as f:
        for sku in sorted(sales):
            weekly = sales[sku] / 2  # two weeks of history around the same week last year
            f.write(json.dumps({"sku": sku, "week_from": today.isoformat(), "units": round(weekly * scale)}, sort_keys=True) + "\n")
    print(json.dumps({"type": "output", "port": "rows", "path": out, "media_type": "application/x-ndjson"}))
    print(json.dumps({"type": "result", "ok": True}))


if __name__ == "__main__":
    main()
