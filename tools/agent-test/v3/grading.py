"""Grading for the agent test, version 3: whether a package an agent changed does what its task
says, judged by what was made and how it runs, with the commands the superpowers plan added.

    grade(task, src, wrela) -> {"built", "correct", "why", "failed"}

`src` is a directory of the package's .wrela files (an attempt's snapshot); it is copied, never
changed. `wrela` is the compiler under test, which runs every check. A task's `grade` in
tasks.json lists its checks, each of which must hold, in this order:

- `tests`: the hidden tests (grades/<id>.wrela, added as zgrade.wrela) pass under `wrela test`;
  `golden`, a PNG in grades/, is copied to the package's goldens/ for a frame test's golden.
- `inspect`: `wrela inspect <pkg> <subject> --region <r> --depth <d> --json` finds `pieces`
  certain pieces and no finding of the kinds in `absent` (with `lift`, `--lift` too).
- `fuzz`: `wrela fuzz <pkg> --runs <runs> --seed <seed> --json` finds nothing (exit 0).
- `pipeline`: in `wrela pipelines <pkg> --json`, the pipeline with entry point `entry` has its
  work's bound at most `work`.
- `same`: `wrela same <pkg> --against <the task's start, built> --within <within> --frames <n>`
  says the two are the same.
"""

import json
import os
import shutil
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
# The file the hidden tests go in.
GRADE = "zgrade.wrela"


def tasks():
    with open(os.path.join(HERE, "tasks.json")) as f:
        return json.load(f)


def start_of(task):
    """The package the task hands the agent."""
    return os.path.join(HERE, "tasks", task["id"], "start")


def reference(task, side):
    """A task's `right` or `wrong` reference: its own package, or (`wrong: "start"`) the start."""
    if side == "wrong" and task.get("wrong") == "start":
        return start_of(task)
    return os.path.join(HERE, "tasks", task["id"], side)


def copy_package(src, dst):
    """The package's .wrela and .toml files and goldens, without build output or attempts."""
    for root, dirs, files in os.walk(src):
        dirs[:] = [d for d in dirs if d not in ("build", "attempts")]
        for name in files:
            if not name.endswith((".wrela", ".toml", ".png")):
                continue
            rel = os.path.relpath(os.path.join(root, name), src)
            os.makedirs(os.path.dirname(os.path.join(dst, rel)), exist_ok=True)
            shutil.copy(os.path.join(root, name), os.path.join(dst, rel))


def _run(args, timeout=900):
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None, "", f"timed out after {timeout} s"
    return out.returncode, out.stdout, out.stderr


def _json(out):
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return None


def _errors(diagnostics):
    return [d for d in diagnostics if d.get("severity") == "error"]


def _tests(task, pkg, wrela, result):
    g = task["grade"]
    shutil.copy(os.path.join(HERE, "grades", task["id"] + ".wrela"), os.path.join(pkg, GRADE))
    if g.get("golden"):
        os.makedirs(os.path.join(pkg, "goldens"), exist_ok=True)
        shutil.copy(os.path.join(HERE, "grades", g["golden"]), os.path.join(pkg, "goldens", g["golden"]))
    code, out, err = _run([wrela, "test", pkg, "--json"])
    report = _json(out)
    if code is None or not isinstance(report, dict):
        return "no test report: " + (err or out)[-600:]
    if _errors(report.get("diagnostics", [])):
        return "it doesn't build: " + "; ".join(
            f'{d["code"]} {d["message"]}' for d in _errors(report["diagnostics"]))[:600]
    result["built"] = True
    ran = report.get("tests", [])
    result["failed"] = [t["name"] for t in ran if not t["passed"]]
    if not any(t["name"].startswith("grade_") for t in ran):
        return "the grade's tests didn't run"
    failed = [t for t in ran if not t["passed"] and t["name"].startswith("grade_")]
    if failed:
        return "hidden tests failed: " + "; ".join(
            f'{t["name"]}: {(t.get("failure") or {}).get("message", "")}' for t in failed)[:600]
    os.remove(os.path.join(pkg, GRADE))
    return None


def _inspect(task, pkg, wrela):
    i = task["grade"]["inspect"]
    args = [wrela, "inspect", pkg, i["subject"], "--region", " " + i["region"],
            "--depth", str(i.get("depth", 6)), "--json"]
    if i.get("lift"):
        args.append("--lift")
    code, out, err = _run(args)
    r = _json(out)
    if not isinstance(r, dict):
        return "wrela inspect gave no report: " + (err or out)[-600:]
    pieces = (r.get("pieces") or {}).get("certain")
    if "pieces" in i and pieces != i["pieces"]:
        return f"wrela inspect finds {pieces} certain pieces, not {i['pieces']}"
    found = [f for f in r.get("findings", []) if f.get("kind") in i.get("absent", [])]
    if found:
        return "wrela inspect finds " + "; ".join(
            f'{f["kind"]} ({", ".join(f.get("parts") or []) or f.get("text") or ""})' for f in found)[:600]
    return None


def _fuzz(task, pkg, wrela):
    z = task["grade"]["fuzz"]
    code, out, err = _run([wrela, "fuzz", pkg, "--runs", str(z["runs"]), "--seed", str(z["seed"]), "--json"])
    if code == 0:
        return None
    r = _json(out)
    if isinstance(r, dict) and r.get("findings"):
        f = r["findings"][0]
        return f'wrela fuzz finds a {f.get("kind")}: {f.get("message", "")}'[:600]
    return "wrela fuzz failed: " + (err or out)[-600:]


def _pipeline(task, pkg, wrela):
    p = task["grade"]["pipeline"]
    code, out, err = _run([wrela, "pipelines", pkg, "--json"])
    r = _json(out)
    if code != 0 or not isinstance(r, dict):
        return "wrela pipelines failed: " + (err or out)[-600:]
    named = [x for x in r["pipelines"] if any(e.split("::")[0] == p["entry"] for e in x["entries"])]
    if not named:
        return f"no pipeline has the entry point `{p['entry']}`"
    for x in named:
        hi = x["work"][1]
        if hi is None:
            return f"`{p['entry']}`'s work has no bound"
        if hi > p["work"]:
            return f"`{p['entry']}`'s work is {hi}, more than {p['work']}"
    return None


def _same(task, pkg, wrela):
    s = task["grade"]["same"]
    with tempfile.TemporaryDirectory(prefix="wrela-start-") as tmp:
        start = os.path.join(tmp, "start")
        copy_package(start_of(task), start)
        built = os.path.join(tmp, "built")
        code, out, err = _run([wrela, "build", start, "-o", built])
        if code != 0:
            return "the task's start doesn't build: " + (err or out)[-600:]
        code, out, err = _run([wrela, "same", pkg, "--against", built, "--within", str(s["within"]),
                               "--frames", str(s.get("frames", 10)), "--json"])
        r = _json(out)
        if isinstance(r, dict) and r.get("same") is True:
            return None
        return "wrela same: it doesn't draw what the start did: " + (out or err)[-600:]


def grade(task, src, wrela):
    result = {"built": False, "correct": False, "why": "", "failed": []}
    if os.path.exists(os.path.join(src, GRADE)):
        result["why"] = f"the program has a {GRADE} of its own, which the grade replaces"
        return result
    with tempfile.TemporaryDirectory(prefix="wrela-grade-") as tmp:
        pkg = os.path.join(tmp, "pkg")
        os.makedirs(pkg)
        copy_package(src, pkg)
        g = task["grade"]
        code, out, err = _run([wrela, "build", pkg, "-o", os.path.join(tmp, "build"), "--json"])
        report = _json(out)
        if code is None or not isinstance(report, dict):
            result["why"] = "no build report: " + (err or out)[-600:]
            return result
        if _errors(report.get("diagnostics", [])):
            result["why"] = "it doesn't build: " + "; ".join(
                f'{d["code"]} {d["message"]}' for d in _errors(report["diagnostics"]))[:600]
            return result
        result["built"] = True
        checks = [("tests", _tests), ("inspect", _inspect), ("fuzz", _fuzz),
                  ("pipeline", _pipeline), ("same", _same)]
        for key, check in checks:
            if key not in g:
                continue
            why = check(task, pkg, wrela, result) if key == "tests" else check(task, pkg, wrela)
            if why:
                result["why"] = why
                return result
        result["correct"] = True
        return result
