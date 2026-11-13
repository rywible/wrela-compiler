#!/usr/bin/env python3
"""Runs the agent test, version 3: one fresh agent per task and arm, through the Claude CLI,
each changing a package that the task starts from, with every attempt graded (grading.py) and
the two arms scored side by side (score.py).

    tools/agent-test/v3/run.py <run-name> --wrela /abs/wrela [--tools with|without|both]
        [--model M] [--tasks id,id] [--parallel n] [--dry-run]
    tools/agent-test/v3/run.py <run-name> --grade [--wrela /abs/wrela]

An arm is the set of commands the agent may run besides compiling (config.json's `tools`):
`without` is version 2's (`wrela doc`, `explain`, `primer`); `with` adds the commands the
superpowers plan added (`wrela pixel`, `inspect`, `sliders`, `fuzz`, `twin`, `solve`, `test`).
`--tools both` (the default) runs each task in each arm. A run lives in
`target/agent-test/<run-name>/<arm>/<task>/` and is never written over. `run.json` records the
date, model, arms, the compiler's path, SHA-256 and version, and the checkout's revision. Each
task's directory holds `BRIEF.txt`, the agent's `pkg/` (a copy of the task's start),
`attempts/N/` (attempt.sh's, each with its `graded.json`), `transcript.jsonl` (the CLI's
stream, from which score.py reads the tokens, the cost and the commands the agent ran) and
`report.md` (the agent's answer).

It spends money: each task and arm runs an agent, at most `budget_usd` from config.json.
`--dry-run` prints each command and the most the run could cost, and launches nothing: no
model or API is called.
"""

import argparse
import datetime
import hashlib
import json
import os
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
sys.path.insert(0, HERE)
import grading  # noqa: E402
import score  # noqa: E402

ARMS = ("with", "without")


def config():
    with open(os.path.join(HERE, "config.json")) as f:
        return json.load(f)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def version(wrela):
    out = subprocess.run([wrela, "--version"], capture_output=True, text=True)
    return out.stdout.strip() or out.stderr.strip()


def compiler(path):
    if not path or not os.path.isabs(path) or not os.access(path, os.X_OK):
        sys.exit(f"run.py: --wrela must be an absolute path to an executable `wrela`, not {path!r}")
    return path


def allowed(cfg, arm, wrela, attempt):
    """The CLI's allowlist for an arm: the files, the attempt script, and the arm's commands."""
    tools = ["Read", "Write", "Edit", f"Bash({attempt}:*)"]
    tools += [f"Bash({wrela} {c}:*)" for c in cfg["tools"][arm]]
    return ",".join(tools)


def brief(cfg, t, arm, task_dir, context, attempt, wrela):
    return cfg["brief"].format(
        context=", ".join(context),
        id=t["id"],
        task=t["task"],
        dir=task_dir,
        tools=", ".join(f"`{wrela} {c} ...`" for c in cfg["tools"][arm]),
        max_attempts=cfg["max_attempts"],
        attempt=attempt,
    )


def claude_command(cfg, model, arm, task_dir, attempt, wrela):
    return [
        "claude", "-p", f"Read {task_dir}/BRIEF.txt and do exactly what it says.",
        "--model", model,
        "--permission-mode", "dontAsk",
        "--allowedTools", allowed(cfg, arm, wrela, attempt),
        "--output-format", "stream-json", "--verbose",
        "--max-budget-usd", str(cfg["budget_usd"]),
        "--no-session-persistence",
    ]


def grade_task(task_dir, t, wrela):
    """Grades each attempt of task `t` in `task_dir` that has no grade yet."""
    attempts = os.path.join(task_dir, "attempts")
    if not os.path.isdir(attempts):
        return
    for n in sorted((d for d in os.listdir(attempts) if d.isdigit()), key=int):
        a = os.path.join(attempts, n)
        g = grading.grade(t, os.path.join(a, "src"), wrela)
        with open(os.path.join(a, "graded.json"), "w") as f:
            json.dump(g, f, indent=2)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("name")
    p.add_argument("--wrela")
    p.add_argument("--tools", default="both", choices=["with", "without", "both"])
    p.add_argument("--model")
    p.add_argument("--tasks")
    p.add_argument("--parallel", type=int, default=2)
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--grade", action="store_true", help="grade a finished run again")
    a = p.parse_args()
    cfg = config()
    run = os.path.join(ROOT, "target", "agent-test", a.name)
    ids = a.tasks.split(",") if a.tasks else None
    tasks = [t for t in grading.tasks() if ids is None or t["id"] in ids]
    arms = ARMS if a.tools == "both" else (a.tools,)
    if a.grade:
        with open(os.path.join(run, "run.json")) as f:
            recorded = json.load(f)
        wrela = compiler(a.wrela or recorded["wrela"]["path"])
        for arm in recorded["arms"]:
            for t in tasks:
                grade_task(os.path.join(run, arm, t["id"]), t, wrela)
        write_score(run)
        return
    wrela = compiler(a.wrela)
    if os.path.exists(run) and not a.dry_run:
        sys.exit(f"run.py: {run} exists: a run is never written over, so choose another name")
    model = a.model or cfg["model"]
    attempt = os.path.join(run, "attempt")
    jobs = []
    for arm in arms:
        for t in tasks:
            task_dir = os.path.join(run, arm, t["id"])
            jobs.append((arm, t, task_dir, claude_command(cfg, model, arm, task_dir, attempt, wrela)))
    if a.dry_run:
        for arm, t, task_dir, cmd in jobs:
            print(f"[{arm}] " + " ".join(json.dumps(c) if " " in c or "," in c else c for c in cmd))
        print(f"{len(tasks)} tasks, {len(arms)} arms: {len(jobs)} agents; "
              f"at most ${len(jobs) * cfg['budget_usd']} at ${cfg['budget_usd']} an agent")
        return
    os.makedirs(run)
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    with open(os.path.join(run, "run.json"), "w") as f:
        json.dump({
            "date": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
            "model": model,
            "arms": list(arms),
            "wrela": {"path": wrela, "sha256": sha256(wrela), "version": version(wrela)},
            "revision": revision,
            "tasks": [t["id"] for t in tasks],
        }, f, indent=2)
    # The one way the agent compiles: attempt.sh with this run's compiler.
    with open(attempt, "w") as f:
        f.write(f'#!/usr/bin/env bash\nexec env WRELA_BIN={json.dumps(wrela)} {json.dumps(os.path.join(HERE, "attempt.sh"))} "$@"\n')
    os.chmod(attempt, 0o755)
    context = [os.path.join(ROOT, c) for c in cfg["context_sets"]["full"]]

    def one(job):
        arm, t, task_dir, cmd = job
        shutil.copytree(grading.start_of(t), os.path.join(task_dir, "pkg"))
        with open(os.path.join(task_dir, "BRIEF.txt"), "w") as f:
            f.write(brief(cfg, t, arm, task_dir, context, attempt, wrela) + "\n")
        out = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, timeout=3600)
        with open(os.path.join(task_dir, "transcript.jsonl"), "w") as f:
            f.write(out.stdout)
        with open(os.path.join(task_dir, "report.md"), "w") as f:
            f.write(score.final_result(out.stdout) or out.stderr)
        grade_task(task_dir, t, wrela)
        print(f"[{arm}] {t['id']}: done", flush=True)

    with ThreadPoolExecutor(max(1, a.parallel)) as ex:
        list(ex.map(one, jobs))
    write_score(run)


def write_score(run):
    """Scores the run's arms side by side: `score.json`, also printed."""
    text = json.dumps(score.report(run), indent=2) + "\n"
    with open(os.path.join(run, "score.json"), "w") as f:
        f.write(text)
    print(text)


if __name__ == "__main__":
    main()
