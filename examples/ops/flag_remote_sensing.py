"""Flags papers whose extracted methods mention remote sensing (illustrative `python` operator).

Input `rows` (JSON Lines from llm.extract): title, abstract, methods. Output `rows`: the same rows
with `uses_remote_sensing` true or false. Pure keyword matching, so it is deterministic and cheap;
the LLM step upstream already did the hard part.
"""

import json
import sys

TERMS = ("remote sensing", "satellite", "sentinel-5p", "tropomi", "modis", "viirs", "aerosol optical depth", "landsat")


def main():
    start = json.loads(sys.stdin.readline())
    out = "flagged.jsonl"
    with open(out, "w", encoding="utf-8") as f:
        for item in start["inputs"]:
            with open(item["path"], encoding="utf-8") as src:
                for line in src:
                    row = json.loads(line)
                    text = " ".join(str(row.get(k, "")) for k in ("title", "abstract", "methods")).lower()
                    row["uses_remote_sensing"] = any(t in text for t in TERMS)
                    f.write(json.dumps(row, sort_keys=True) + "\n")
    print(json.dumps({"type": "output", "port": "rows", "path": out, "media_type": "application/x-ndjson"}))
    print(json.dumps({"type": "result", "ok": True}))


if __name__ == "__main__":
    main()
