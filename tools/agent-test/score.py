#!/usr/bin/env python3
"""Scores an agent syntax test run from its attempt logs (attempt.sh).

    tools/agent-test/score.py <run-dir> [--misread <file.json>]

<run-dir> holds one directory per task (named by its id), each with attempts/N/{status,
diagnostics.json}. Prints the report as JSON: per task, the attempts to the first build and the
codes seen; overall, the first-try build rate, the mean attempts to success, and the codes
agents met. `--misread` merges the reviewer's list of diagnostics the agents misread.

An attempt that never finished (stopped before it wrote its status) counts as one that didn't
build, and is listed as unfinished. Anything in attempts/ that isn't a numbered directory (such as
.DS_Store) is ignored.
"""
import json, os, sys
from collections import Counter

def read(path):
    with open(path) as f:
        return f.read()

def read_json(path):
    with open(path) as f:
        return json.load(f)

def attempt(a):
    """An attempt's status ("0" if it built; None if it never finished) and error codes."""
    try:
        status = read(os.path.join(a, "status")).strip()
    except FileNotFoundError:
        status = None
    try:
        diags = read_json(os.path.join(a, "diagnostics.json")).get("diagnostics", [])
    except (FileNotFoundError, json.JSONDecodeError):
        diags = []
    return status, sorted({x["code"] for x in diags if x.get("severity") == "error"})

def main():
    run = sys.argv[1]
    misread = []
    if "--misread" in sys.argv:
        misread = read_json(sys.argv[sys.argv.index("--misread") + 1])
    tasks = read_json(os.path.join(os.path.dirname(__file__), "tasks.json"))
    per = []
    for t in tasks:
        d = os.path.join(run, t["id"], "attempts")
        names = os.listdir(d) if os.path.isdir(d) else []
        attempts = sorted(int(n) for n in names if n.isdigit() and os.path.isdir(os.path.join(d, n)))
        codes, success, unfinished = [], None, []
        for n in attempts:
            status, seen = attempt(os.path.join(d, str(n)))
            codes.append(seen)
            if status is None:
                unfinished.append(n)
            if status == "0" and success is None:
                success = n
        per.append({"task": t["id"], "attempts": len(attempts), "built_at": success,
                    "codes_per_attempt": codes, "unfinished": unfinished})
    built = [p for p in per if p["built_at"] is not None]
    first_try = sum(1 for p in per if p["built_at"] == 1)
    seen = Counter(c for p in per for cs in p["codes_per_attempt"] for c in cs)
    report = {
        "tasks": len(per),
        "first_try_builds": first_try,
        "first_try_rate": round(first_try / len(per), 3),
        "built_eventually": len(built),
        "mean_attempts_to_success": round(sum(p["built_at"] for p in built) / max(len(built), 1), 2),
        "codes_met": dict(seen.most_common()),  # most first; ties in the order first met
        "misread": misread,
        "per_task": per,
    }
    print(json.dumps(report, indent=2))

if __name__ == "__main__":
    main()
