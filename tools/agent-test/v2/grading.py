"""Grading for the agent syntax test, version 2: whether a program an agent wrote does what its
task says, by hidden tests of the exports the task names (grades/<id>.wrela, added to the
package as zgrade.wrela: a module whose file sorts after main.wrela, as toolchains before 0.5.0
need for a frame test outside main to take the state) or by the pipelines it must reach
(`wrela pipelines --json`).

    grade(task, src, wrela, validator=None) -> {"built", "correct", "why", "failed"}

`src` is a directory of the program's .wrela files (an attempt's snapshot); it is copied, never
changed. `wrela` builds and runs it; `validator`, a fixed compiler, checks the criteria that
depend on a lint (task 25's W0011), so they are the same whichever compiler is under test.
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


def task(id):
    return next(t for t in tasks() if t["id"] == id)


def _copy(src, dst):
    for root, dirs, files in os.walk(src):
        dirs[:] = [d for d in dirs if d not in ("build", "attempts")]
        for name in files:
            if not name.endswith(".wrela"):
                continue
            rel = os.path.relpath(os.path.join(root, name), src)
            os.makedirs(os.path.dirname(os.path.join(dst, rel)), exist_ok=True)
            shutil.copy(os.path.join(root, name), os.path.join(dst, rel))


def _run(args, timeout=300):
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None, "", f"timed out after {timeout} s"
    return out.returncode, out.stdout, out.stderr


def _errors(diagnostics):
    return [d for d in diagnostics if d.get("severity") == "error"]


def grade(task, src, wrela, validator=None):
    validator = validator or wrela
    result = {"built": False, "correct": False, "why": "", "failed": []}
    if os.path.exists(os.path.join(src, GRADE)):
        result["why"] = f"the program has a {GRADE} of its own, which the grade replaces"
        return result
    with tempfile.TemporaryDirectory(prefix="wrela-grade-") as tmp:
        pkg = os.path.join(tmp, "pkg")
        os.makedirs(pkg)
        _copy(src, pkg)
        if "pipelines" in task["grade"]:
            code, out, err = _run([wrela, "pipelines", pkg, "--json"])
            if code != 0:
                result["why"] = "it doesn't build: " + (err or out)[-600:]
                return result
            result["built"] = True
            kinds = {p["kind"] for p in json.loads(out)["pipelines"]}
            missing = [k for k in task["grade"]["pipelines"] if k not in kinds]
            if missing:
                result["why"] = "it reaches no " + " and no ".join(f"{k} pipeline" for k in missing)
                return result
            result["correct"] = True
            return result
        shutil.copy(os.path.join(HERE, "grades", task["id"] + ".wrela"), os.path.join(pkg, GRADE))
        code, out, err = _run([wrela, "test", pkg, "--json"])
        if code is None:
            result["why"] = err
            return result
        try:
            report = json.loads(out)
        except json.JSONDecodeError:
            result["why"] = "no report: " + err[-600:]
            return result
        if _errors(report.get("diagnostics", [])):
            result["why"] = "it doesn't build: " + "; ".join(
                f'{d["code"]} {d["message"]}' for d in _errors(report["diagnostics"])
            )[:600]
            return result
        result["built"] = True
        ran = report.get("tests", [])
        failed = [t["name"] for t in ran if not t["passed"]]
        result["failed"] = failed
        if not any(t["name"].startswith("grade_") for t in ran):
            result["why"] = "the grade's tests didn't run"
            return result
        if failed:
            result["why"] = "tests failed: " + ", ".join(failed)
            return result
        if task["id"] == "25-write-a-test":
            if not any(t["name"] == "fib_is_right" and t["passed"] for t in ran):
                result["why"] = "no passing test named `fib_is_right`"
                return result
            os.remove(os.path.join(pkg, GRADE))
            code, out, _ = _run([validator, "check", pkg, "--json"])
            codes = [d["code"] for d in json.loads(out or "{}").get("diagnostics", [])]
            if "W0011" in codes:
                result["why"] = "its test can't fail (W0011): it asserts nothing"
                return result
        result["correct"] = True
        return result
