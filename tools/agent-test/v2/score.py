#!/usr/bin/env python3
"""Scores a run of the agent syntax test, version 2 (run.py), from its attempt logs.

    tools/agent-test/v2/score.py <run-dir>

<run-dir> holds one directory per task, each with attempts/N/{status, diagnostics.json,
graded.json} and the CLI's usage.json. Prints the report as JSON: per task, the attempt that
first built, the attempt that was first correct (its graded.json says so), the codes met, the
tokens and the cost; overall, the first-try correct and build rates, the means, and the codes
met. Attempts past config.json's `max_attempts` don't count; an attempt that never finished
counts as one that didn't build.
"""

import json
import os
import sys
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))


def read_json(path, default=None):
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return default


def tokens_and_cost(usage):
    """The tokens and the cost the CLI's JSON report gives (0 if it gives none)."""
    if not isinstance(usage, dict):
        return 0, 0.0
    u = usage.get("usage") or {}
    tokens = sum(int(u.get(k) or 0) for k in (
        "input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))
    return tokens, float(usage.get("total_cost_usd") or 0.0)


def main():
    run = sys.argv[1]
    tasks = read_json(os.path.join(HERE, "tasks.json"))
    # The tasks the run ran (run.json says), else every task.
    ran = read_json(os.path.join(run, "run.json"), {}).get("tasks")
    if ran:
        tasks = [t for t in tasks if t["id"] in ran]
    limit = read_json(os.path.join(HERE, "config.json"))["max_attempts"]
    per = []
    for t in tasks:
        d = os.path.join(run, t["id"], "attempts")
        names = os.listdir(d) if os.path.isdir(d) else []
        numbered = sorted(int(n) for n in names if n.isdigit() and os.path.isdir(os.path.join(d, n)))
        attempts = [n for n in numbered if n <= limit]
        built_at = correct_at = None
        codes, graded = [], []
        for n in attempts:
            a = os.path.join(d, str(n))
            try:
                with open(os.path.join(a, "status")) as f:
                    status = f.read().strip()
            except FileNotFoundError:
                status = None
            diags = read_json(os.path.join(a, "diagnostics.json"), {}).get("diagnostics", [])
            codes.append(sorted({x["code"] for x in diags if x.get("severity") == "error"}))
            g = read_json(os.path.join(a, "graded.json"), {})
            graded.append(g.get("why", "") if not g.get("correct") else "")
            if status == "0" and built_at is None:
                built_at = n
            if g.get("correct") and correct_at is None:
                correct_at = n
        tokens, cost = tokens_and_cost(read_json(os.path.join(run, t["id"], "usage.json")))
        per.append({"task": t["id"], "attempts": len(attempts), "built_at": built_at,
                    "correct_at": correct_at, "codes_per_attempt": codes, "graded": graded,
                    "tokens": tokens, "cost_usd": round(cost, 4),
                    "over_limit": [n for n in numbered if n > limit]})
    n = max(len(per), 1)
    correct = [p for p in per if p["correct_at"] is not None]
    built = [p for p in per if p["built_at"] is not None]
    report = {
        "tasks": len(per),
        "first_try_correct": sum(1 for p in per if p["correct_at"] == 1),
        "first_try_correct_rate": round(sum(1 for p in per if p["correct_at"] == 1) / n, 3),
        "correct_eventually": len(correct),
        "mean_attempts_to_correct": round(sum(p["correct_at"] for p in correct) / max(len(correct), 1), 2),
        "first_try_builds": sum(1 for p in per if p["built_at"] == 1),
        "built_eventually": len(built),
        "mean_tokens": round(sum(p["tokens"] for p in per) / n),
        "mean_cost_usd": round(sum(p["cost_usd"] for p in per) / n, 4),
        "codes_met": dict(Counter(c for p in per for cs in p["codes_per_attempt"] for c in cs).most_common()),
        "per_task": per,
    }
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
