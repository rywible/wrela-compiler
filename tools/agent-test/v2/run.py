#!/usr/bin/env python3
"""Runs the agent syntax test, version 2: one fresh agent per task, through the Claude CLI, each
writing a program from the spec alone, with every attempt graded (grading.py) and the run
scored (score.py).

    tools/agent-test/v2/run.py <run-name> --wrela /abs/wrela [--validator /abs/wrela]
        [--model M] [--context full|core] [--tasks id,id] [--parallel n] [--dry-run]
    tools/agent-test/v2/run.py <run-name> --grade [--wrela /abs/wrela] [--validator ...]
    tools/agent-test/v2/run.py <run-name> --resume

The compiler is chosen once, explicitly: `--wrela` builds every attempt (through attempt.sh,
which never builds a compiler), runs the hidden tests and answers the agent's `doc`, `explain`
and `primer`. `--validator` (default: `--wrela`) checks the criteria that depend on a lint, so
they are the same in runs of different compilers that are compared. A run lives in
`target/agent-test/<run-name>/` and is never written over: a run name that exists is refused.
`run.json` records the date, model, context set, each compiler's path, SHA-256 and version, and
the checkout's revision. `--resume` finishes a run that stopped (a full disk, an interrupt):
each of its tasks without the CLI's report runs again from nothing, with the run's own
compiler, which must still have the SHA-256 recorded, and each attempt of the others without
its `graded.json` is graded. `--grade` and `--resume` write `score.json` again.
Each task's directory holds `BRIEF.txt`, the agent's `pkg/`,
`attempts/N/` (attempt.sh's, each with its `graded.json`), `usage.json` (the CLI's report, with
tokens and cost) and `report.md` (the agent's answer).

It spends money: each task runs an agent, at most `budget_usd` from config.json. `--dry-run`
prints each command and launches nothing.
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


def compiler(path, what):
    if not path or not os.path.isabs(path) or not os.access(path, os.X_OK):
        sys.exit(f"run.py: {what} must be an absolute path to an executable `wrela`, not {path!r}")
    return path


def core_language(cfg, out):
    """The core context set's spec: the sections config.json names, from spec/language.md."""
    with open(os.path.join(ROOT, "spec/language.md")) as f:
        text = f.read()
    lines = text.split("\n")
    keep = []
    on = False
    for line in lines:
        if line.startswith("## "):
            title = line[3:].strip()
            on = any(title == s or title.startswith(s) for s in cfg["core_sections"])
        if on or line.startswith("# The Wrela language"):
            keep.append(line)
    with open(out, "w") as f:
        f.write("\n".join(keep) + "\n")


# An agent's Read tool shows a long file a window at a time, so a large context file is given in
# parts small enough to be read whole at once: each a run of its `## ` sections under this.
PART_BYTES = 60_000


def readable(path, run):
    """`path`, or its parts in `run` when it's too large to read whole: split at its `## `
    headings, in order, each part at most PART_BYTES where its sections allow."""
    if os.path.getsize(path) <= PART_BYTES:
        return [path]
    with open(path) as f:
        text = f.read()
    sections, cur = [], []
    for line in text.split("\n"):
        if line.startswith("## ") and cur:
            sections.append("\n".join(cur) + "\n")
            cur = []
        cur.append(line)
    sections.append("\n".join(cur))
    parts, part = [], ""
    for sec in sections:
        if part and len((part + sec).encode()) > PART_BYTES:
            parts.append(part)
            part = ""
        part += sec
    parts.append(part)
    stem = os.path.splitext(os.path.basename(path))[0]
    out = []
    for i, text in enumerate(parts, 1):
        p = os.path.join(run, f"{stem}-part{i}-of-{len(parts)}.md")
        with open(p, "w") as f:
            f.write(text)
        out.append(p)
    return out


def brief(cfg, t, task_dir, context, attempt, wrela):
    return cfg["brief"].format(
        context=", ".join(context),
        id=t["id"],
        task=t["task"],
        dir=task_dir,
        max_attempts=cfg["max_attempts"],
        attempt=attempt,
        wrela=wrela,
    )


def claude_command(cfg, model, task_dir, attempt, wrela):
    tools = ",".join(
        ["Read", "Write", "Edit", f"Bash({attempt}:*)", f"Bash({wrela} doc:*)",
         f"Bash({wrela} explain:*)", f"Bash({wrela} primer:*)"]
    )
    return [
        "claude", "-p", f"Read {task_dir}/BRIEF.txt and do exactly what it says.",
        "--model", model,
        "--permission-mode", "dontAsk",
        "--allowedTools", tools,
        "--output-format", "json",
        "--max-budget-usd", str(cfg["budget_usd"]),
        "--no-session-persistence",
    ]


def grade_run(run, wrela, validator, ids=None):
    """Grades every attempt of the run's tasks: each attempt's `graded.json`."""
    for t in grading.tasks():
        if not ids or t["id"] in ids:
            grade_task(run, t, wrela, validator)


def graded(attempt):
    """Whether an attempt's directory holds its grade, whole."""
    try:
        with open(os.path.join(attempt, "graded.json")) as f:
            return isinstance(json.load(f), dict)
    except (FileNotFoundError, json.JSONDecodeError):
        return False


def grade_task(run, t, wrela, validator, missing=False):
    """Grades every attempt of the run's task `t`, or with `missing` each one without its grade:
    each attempt's `graded.json`."""
    attempts = os.path.join(run, t["id"], "attempts")
    if not os.path.isdir(attempts):
        return
    for n in sorted((d for d in os.listdir(attempts) if d.isdigit()), key=int):
        a = os.path.join(attempts, n)
        if missing and graded(a):
            continue
        g = grading.grade(t, os.path.join(a, "src"), wrela, validator)
        with open(os.path.join(a, "graded.json"), "w") as f:
            json.dump(g, f, indent=2)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("name")
    p.add_argument("--wrela")
    p.add_argument("--validator")
    p.add_argument("--model")
    p.add_argument("--context", default="full", choices=["full", "core"])
    p.add_argument("--tasks")
    p.add_argument("--parallel", type=int, default=2)
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--grade", action="store_true", help="grade a finished run again")
    p.add_argument("--resume", action="store_true", help="run a stopped run's unfinished tasks")
    a = p.parse_args()
    cfg = config()
    run = os.path.join(ROOT, "target", "agent-test", a.name)
    ids = a.tasks.split(",") if a.tasks else None
    if a.grade:
        with open(os.path.join(run, "run.json")) as f:
            recorded = json.load(f)
        wrela = compiler(a.wrela or recorded["wrela"]["path"], "--wrela")
        validator = compiler(a.validator or recorded["validator"]["path"], "--validator")
        grade_run(run, wrela, validator, ids)
        write_score(run)
        return
    if a.resume:
        with open(os.path.join(run, "run.json")) as f:
            recorded = json.load(f)
        wrela, validator = recorded["wrela"]["path"], recorded["validator"]["path"]
        for which in ("wrela", "validator"):
            if sha256(recorded[which]["path"]) != recorded[which]["sha256"]:
                sys.exit(f"run.py: the run's {which} has changed since it started; resume it with that one")
        a.model, a.context = recorded["model"], recorded["context"]

        def finished(id):
            try:
                with open(os.path.join(run, id, "usage.json")) as f:
                    return isinstance(json.load(f), dict)
            except (FileNotFoundError, json.JSONDecodeError):
                return False

        ids = [id for id in recorded["tasks"] if not finished(id)]
        if not ids:
            print("nothing to resume")
    else:
        wrela = compiler(a.wrela, "--wrela")
        validator = compiler(a.validator or wrela, "--validator")
        if os.path.exists(run):
            sys.exit(f"run.py: {run} exists: a run is never written over, so choose another name")
    tasks = [t for t in grading.tasks() if ids is None or t["id"] in ids]
    model = a.model or cfg["model"]
    attempt = os.path.join(run, "attempt")
    commands = []
    for t in tasks:
        task_dir = os.path.join(run, t["id"])
        commands.append((t, task_dir, claude_command(cfg, model, task_dir, attempt, wrela)))
    if a.dry_run:
        for t, task_dir, cmd in commands:
            print(" ".join(json.dumps(c) if " " in c else c for c in cmd))
        print(f"{len(commands)} tasks; at most ${len(commands) * cfg['budget_usd']} at {cfg['budget_usd']} a task")
        return
    if a.resume:
        for id in ids:
            shutil.rmtree(os.path.join(run, id), ignore_errors=True)
        # A task whose agent finished may have stopped while its attempts were graded.
        for t in grading.tasks():
            if t["id"] in recorded["tasks"] and t["id"] not in ids:
                grade_task(run, t, wrela, validator, missing=True)
        return run_tasks(a, cfg, run, commands, wrela, validator, attempt)
    os.makedirs(run)
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    record = {
        "date": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "model": model,
        "context": a.context,
        "wrela": {"path": wrela, "sha256": sha256(wrela), "version": version(wrela)},
        "validator": {"path": validator, "sha256": sha256(validator), "version": version(validator)},
        "revision": revision,
        "tasks": [t["id"] for t in tasks],
    }
    with open(os.path.join(run, "run.json"), "w") as f:
        json.dump(record, f, indent=2)
    # The one way the agent compiles: attempt.sh with this run's compiler.
    with open(attempt, "w") as f:
        f.write(f'#!/usr/bin/env bash\nexec env WRELA_BIN={json.dumps(wrela)} {json.dumps(os.path.join(HERE, "attempt.sh"))} "$@"\n')
    os.chmod(attempt, 0o755)
    run_tasks(a, cfg, run, commands, wrela, validator, attempt)


def run_tasks(a, cfg, run, commands, wrela, validator, attempt):
    """Runs each task's agent, grades its attempts, and scores the run."""
    if a.context == "core":
        core_language(cfg, os.path.join(run, "core-language.md"))
    context = []
    for c in cfg["context_sets"][a.context]:
        path = c.replace("{run}", run) if c.startswith("{run}") else os.path.join(ROOT, c)
        context.extend(readable(path, run))

    def one(job):
        t, task_dir, cmd = job
        os.makedirs(os.path.join(task_dir, "pkg"))
        with open(os.path.join(task_dir, "BRIEF.txt"), "w") as f:
            f.write(brief(cfg, t, task_dir, context, attempt, wrela) + "\n")
        out = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, timeout=3600)
        with open(os.path.join(task_dir, "usage.json"), "w") as f:
            f.write(out.stdout)
        try:
            report = json.loads(out.stdout).get("result", "")
        except json.JSONDecodeError:
            report = out.stdout + out.stderr
        with open(os.path.join(task_dir, "report.md"), "w") as f:
            f.write(report or "")
        grade_task(run, t, wrela, validator)
        print(f"{t['id']}: done", flush=True)

    with ThreadPoolExecutor(max(1, a.parallel)) as ex:
        list(ex.map(one, commands))
    write_score(run)


def write_score(run):
    """Scores the run from its attempts' grades: `score.json`, also printed."""
    text = json.dumps(score.report(run), indent=2) + "\n"
    with open(os.path.join(run, "score.json"), "w") as f:
        f.write(text)
    print(text)


if __name__ == "__main__":
    main()
