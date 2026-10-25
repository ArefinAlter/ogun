"""Writes the sample files the hosted hub's workflows fetch (files.example.org inside the compose network).

Deterministic: the same files every time, so the digests in a reset hub are the same as before. Run it from this
directory: python generate.py
"""
import json
import random
from pathlib import Path

OUT = Path(__file__).with_name('files')
STATIONS = [
    (101, 'North', 'Harbour Road'), (102, 'North', 'Mill Lane'), (103, 'North', 'Station Square'),
    (201, 'Centre', 'Market Street'), (202, 'Centre', 'Library Walk'), (203, 'Centre', 'Bridge Street'),
    (204, 'Centre', 'Town Hall'), (301, 'East', 'Canal Side'), (302, 'East', 'Park Gate'),
    (303, 'East', 'Orchard Row'), (401, 'South', 'College Green'), (402, 'South', 'Riverside'),
    (403, 'South', 'Hospital'), (404, 'South', 'Sports Ground'),
]


def write(rel: str, text: str) -> None:
    p = OUT / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text, encoding='utf-8', newline='\n')


def main() -> None:
    rnd = random.Random(20261025)
    write('robots.txt', 'User-agent: *\nAllow: /\n')
    write('index.html', '<!doctype html><title>Sample files</title><p>Sample files for the hosted Ogun pre-alpha hub.</p>\n')
    for year in range(2023, 2027):
        for month in range(1, 13):
            days = 28 if month == 2 else 30 if month in (4, 6, 9, 11) else 31
            lines = ['date,station_id,trips']
            season = 1.0 + 0.4 * (month in (5, 6, 7, 8, 9))
            for day in range(1, days + 1):
                for sid, _, _ in STATIONS:
                    lines.append(f'{year:04}-{month:02}-{day:02},{sid},{int(rnd.randint(20, 90) * season)}')
            write(f'open-trips/trips-{year:04}-{month:02}.csv', '\n'.join(lines) + '\n')
    lines = ['hour,station_id,bikes_min,bikes_mean,docks_min']
    for h in range(30 * 24):
        for sid, _, _ in STATIONS:
            m = rnd.randint(2, 14)
            lines.append(f'{h},{sid},{max(0, m - rnd.randint(0, 3))},{m},{rnd.randint(0, 6)}')
    write('docks/hourly-30d.csv', '\n'.join(lines) + '\n')
    lines = ['station_id,area,street,bikes,empty_risk_pct,expected_by']
    for sid, area, street in STATIONS:
        risk = rnd.randint(35, 96)
        hour = rnd.randint(7, 19)
        lines.append(f'{sid},{area},{street},{rnd.randint(0, 9)},{risk},{hour:02}:{rnd.choice(["00", "15", "30", "45"])}' if risk >= 70 else f'{sid},{area},{street},{rnd.randint(3, 15)},{risk},')
    write('rebalance/shortlist.csv', '\n'.join(lines) + '\n')
    status = {'last_updated': 1761350400, 'ttl': 60, 'data': {'stations': [
        {'station_id': str(sid), 'num_bikes_available': rnd.randint(0, 15), 'num_docks_available': rnd.randint(0, 12), 'is_renting': True, 'is_returning': True}
        for sid, _, _ in STATIONS]}}
    write('gbfs/station_status.json', json.dumps(status, indent=1) + '\n')


if __name__ == '__main__':
    main()
