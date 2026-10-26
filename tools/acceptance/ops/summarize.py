"""A code operator for the acceptance run (O-03, protocol jsonl-stdio/0).

It summarises its input rows and, from inside the sandbox, tries the things a code operator must not be able to do
and reports what happened. The checks are data (config), so the same operator runs unchanged anywhere:

  probe_dir    a folder outside the operator's scratch directory that it may not list (the person's profile)
  outside_dir  a folder outside scratch where it must not be able to create a file
  secret_file  a file holding a canary the operator must not read
  probe_addr   host:port of a loopback listener that must not be reachable

Output (`rows`, JSON Lines): one summary row and one row per check.
"""

import json
import os
import socket
import sys


def probe(name, fn, out):
    try:
        fn()
        out.append({"check": name, "result": "allowed"})
    except Exception as e:  # noqa: BLE001 - the point is to report any refusal
        out.append({"check": name, "result": "blocked", "error": type(e).__name__})


def main():
    start = json.loads(sys.stdin.readline())
    cfg = start.get("config", {})
    rows = []
    for item in start["inputs"]:
        with open(item["path"], encoding="utf-8") as f:
            rows.extend(json.loads(line) for line in f if line.strip())
    checks = []
    if cfg.get("probe_dir"):
        probe("list_the_person_profile", lambda: os.listdir(cfg["probe_dir"]), checks)
    if cfg.get("outside_dir"):
        probe("write_outside_scratch", lambda: open(os.path.join(cfg["outside_dir"], "planted.txt"), "w").write("x"), checks)
    if cfg.get("secret_file"):
        probe("read_a_file_nobody_granted", lambda: open(cfg["secret_file"], encoding="utf-8").read(), checks)
    if cfg.get("probe_addr"):
        host, _, port = cfg["probe_addr"].rpartition(":")
        probe("connect_to_loopback", lambda: socket.create_connection((host, int(port)), timeout=3).close(), checks)
    probe("write_in_scratch", lambda: open("scratch-ok.txt", "w").write("ok"), checks)
    out_path = "summary.jsonl"
    with open(out_path, "w", encoding="utf-8") as out:
        out.write(json.dumps({"rows": len(rows), "prerelease": sum(1 for r in rows if r.get("prerelease") is True), "python": sys.version.split()[0]}, sort_keys=True) + "\n")
        for c in checks:
            out.write(json.dumps(c, sort_keys=True) + "\n")
    print(json.dumps({"type": "output", "port": "rows", "path": out_path, "media_type": "application/x-ndjson"}))
    print(json.dumps({"type": "result", "ok": True}))


if __name__ == "__main__":
    main()
