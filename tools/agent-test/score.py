#!/usr/bin/env python3
"""Scores an agent syntax test run from its attempt logs (attempt.sh).

    tools/agent-test/score.py <run-dir> [--misread <file.json>]

<run-dir> holds one directory per task (named by its id), each with attempts/N/{status,
diagnostics.json}. Prints the report as JSON: per task, the attempts to the first build and the
codes seen; overall, the first-try build rate, the mean attempts to success, and the codes
agents met. `--misread` merges the reviewer's list of diagnostics the agents misread.
"""
import json, os, sys

def main():
    run = sys.argv[1]
    misread = []
    if "--misread" in sys.argv:
        misread = json.load(open(sys.argv[sys.argv.index("--misread") + 1]))
    tasks = json.load(open(os.path.join(os.path.dirname(__file__), "tasks.json")))
    per = []
    for t in tasks:
        d = os.path.join(run, t["id"], "attempts")
        attempts = sorted(int(n) for n in os.listdir(d)) if os.path.isdir(d) else []
        codes, success = [], None
        for n in attempts:
            a = os.path.join(d, str(n))
            status = open(os.path.join(a, "status")).read().strip()
            diags = json.load(open(os.path.join(a, "diagnostics.json"))).get("diagnostics", [])
            codes.append(sorted({x["code"] for x in diags if x["severity"] == "error"}))
            if status == "0" and success is None:
                success = n
        per.append({"task": t["id"], "attempts": len(attempts), "built_at": success,
                    "codes_per_attempt": codes})
    built = [p for p in per if p["built_at"] is not None]
    seen = {}
    for p in per:
        for cs in p["codes_per_attempt"]:
            for c in cs:
                seen[c] = seen.get(c, 0) + 1
    report = {
        "tasks": len(per),
        "first_try_builds": sum(1 for p in per if p["built_at"] == 1),
        "first_try_rate": round(sum(1 for p in per if p["built_at"] == 1) / len(per), 3),
        "built_eventually": len(built),
        "mean_attempts_to_success": round(sum(p["built_at"] for p in built) / max(len(built), 1), 2),
        "codes_met": dict(sorted(seen.items(), key=lambda kv: -kv[1])),
        "misread": misread,
        "per_task": per,
    }
    print(json.dumps(report, indent=2))

main()
