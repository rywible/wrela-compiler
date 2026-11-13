#!/usr/bin/env python3
"""Scores a run of the agent test, version 3 (run.py): its arms side by side.

    tools/agent-test/v3/score.py <run-dir>

`report(run_dir)` gives the same report, as a dict. <run-dir> holds one directory per arm
(`with`, `without`), each with one per task: attempts/N/{status, graded.json} and the CLI's
transcript.jsonl. Per arm: how many tasks were correct, the mean attempts to correct, the
tokens and the cost, and, per task, the attempt first correct, the tokens, the cost and the
`wrela` commands the agent ran (by subcommand). Attempts past config.json's `max_attempts`
don't count.
"""

import json
import os
import sys
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import grading  # noqa: E402


def read_json(path, default=None):
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return default


def events(transcript):
    """The CLI's stream-json events, one a line; lines that aren't JSON are skipped."""
    out = []
    for line in transcript.splitlines():
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return out


def final_result(transcript):
    """The agent's answer: the stream's `result` event's text."""
    for e in events(transcript):
        if e.get("type") == "result":
            return e.get("result", "")
    return ""


def tokens_and_cost(transcript):
    """The tokens and the cost the stream's `result` event gives (0 if none)."""
    for e in events(transcript):
        if e.get("type") == "result":
            u = e.get("usage") or {}
            tokens = sum(int(u.get(k) or 0) for k in (
                "input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))
            return tokens, float(e.get("total_cost_usd") or 0.0)
    return 0, 0.0


def commands(transcript):
    """How many times the agent ran each `wrela` subcommand (and the attempt script), from the
    Bash tool calls in the stream."""
    seen = Counter()
    for e in events(transcript):
        if e.get("type") != "assistant":
            continue
        for c in (e.get("message") or {}).get("content") or []:
            if c.get("type") != "tool_use" or c.get("name") != "Bash":
                continue
            cmd = (c.get("input") or {}).get("command", "").split()
            if not cmd:
                continue
            if cmd[0].endswith("/attempt"):
                seen["attempt"] += 1
            elif os.path.basename(cmd[0]) == "wrela" and len(cmd) > 1:
                seen[cmd[1]] += 1
    return dict(seen)


def arm_report(run, arm, tasks, limit):
    per = []
    for t in tasks:
        d = os.path.join(run, arm, t["id"])
        attempts_dir = os.path.join(d, "attempts")
        names = os.listdir(attempts_dir) if os.path.isdir(attempts_dir) else []
        attempts = sorted(int(n) for n in names if n.isdigit() and int(n) <= limit)
        correct_at = None
        for n in attempts:
            if read_json(os.path.join(attempts_dir, str(n), "graded.json"), {}).get("correct"):
                correct_at = n
                break
        try:
            with open(os.path.join(d, "transcript.jsonl")) as f:
                transcript = f.read()
        except FileNotFoundError:
            transcript = ""
        tokens, cost = tokens_and_cost(transcript)
        per.append({"task": t["id"], "kind": t["kind"], "attempts": len(attempts),
                    "correct_at": correct_at, "tokens": tokens, "cost_usd": round(cost, 4),
                    "commands": commands(transcript)})
    n = max(len(per), 1)
    correct = [p for p in per if p["correct_at"] is not None]
    kinds = Counter(p["kind"] for p in per)
    return {
        "tasks": len(per),
        "correct": len(correct),
        "correct_by_kind": {k: sum(1 for p in correct if p["kind"] == k) for k in kinds},
        "mean_attempts_to_correct": round(sum(p["correct_at"] for p in correct) / max(len(correct), 1), 2),
        "mean_tokens": round(sum(p["tokens"] for p in per) / n),
        "mean_cost_usd": round(sum(p["cost_usd"] for p in per) / n, 4),
        "commands": dict(sum((Counter(p["commands"]) for p in per), Counter())),
        "per_task": per,
    }


def report(run):
    """The report of the run in the directory `run`: each arm's, side by side."""
    recorded = read_json(os.path.join(run, "run.json"), {})
    tasks = grading.tasks()
    if recorded.get("tasks"):
        tasks = [t for t in tasks if t["id"] in recorded["tasks"]]
    limit = read_json(os.path.join(HERE, "config.json"))["max_attempts"]
    arms = recorded.get("arms") or [a for a in ("with", "without") if os.path.isdir(os.path.join(run, a))]
    return {arm: arm_report(run, arm, tasks, limit) for arm in arms}


def main():
    print(json.dumps(report(sys.argv[1]), indent=2))


if __name__ == "__main__":
    main()
