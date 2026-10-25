"""Tests for the agent syntax test, version 2 (tools/agent-test/v2): every task's grade passes
its right reference and fails its wrong one; the runner runs exactly the compiler it's given;
and the score counts attempts to a correct program.

    python3 -m unittest discover -s tools/tests

The references are graded with the compiler at `WRELA_BIN` (an absolute path; `tools/check.sh`
gives the release compiler it builds), checking lint criteria with `WRELA_VALIDATOR` (default
`WRELA_BIN`); without `WRELA_BIN`, this checkout's release compiler, built first.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor

TESTS = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(TESTS))
V2 = os.path.join(ROOT, "tools", "agent-test", "v2")
sys.path.insert(0, V2)
import grading  # noqa: E402

_built = []


def wrela():
    """The compiler under test: `WRELA_BIN`, or this checkout's, built once."""
    if os.environ.get("WRELA_BIN"):
        return os.environ["WRELA_BIN"]
    if not _built:
        subprocess.run(["cargo", "build", "-q", "--release", "-p", "wrela"], cwd=ROOT, check=True)
        target = os.environ.get("CARGO_TARGET_DIR") or os.path.join(ROOT, "target")
        _built.append(os.path.join(os.path.abspath(target), "release", "wrela"))
    return _built[0]


def grade_all(side):
    w = wrela()
    v = os.environ.get("WRELA_VALIDATOR") or w
    def one(t):
        src = os.path.join(V2, "reference", t["id"], side)
        return t["id"], grading.grade(t, src, w, v)
    with ThreadPoolExecutor(8) as ex:
        return list(ex.map(one, grading.tasks()))


class References(unittest.TestCase):
    def test_every_task_has_its_grade_and_references(self):
        tasks = grading.tasks()
        self.assertEqual(len(tasks), 30)
        for t in tasks:
            for side in ("right", "wrong"):
                self.assertTrue(os.path.isfile(os.path.join(V2, "reference", t["id"], side, "main.wrela")), t["id"])
            if "tests" in t["grade"]:
                self.assertTrue(os.path.isfile(os.path.join(V2, "grades", t["id"] + ".wrela")), t["id"])

    def test_every_right_reference_passes_its_grade(self):
        bad = [(id, g["why"]) for id, g in grade_all("right") if not g["correct"]]
        self.assertEqual(bad, [], "a right reference that fails its grade: rewrite the task or the grade")

    def test_every_wrong_reference_fails_its_grade(self):
        results = grade_all("wrong")
        passed = [id for id, g in results if g["correct"]]
        self.assertEqual(passed, [], "a wrong reference that passes its grade: tighten the grade")
        unbuilt = [(id, g["why"]) for id, g in results if not g["built"]]
        self.assertEqual(unbuilt, [], "a wrong reference should build, so its grade is what fails it")


class Runner(unittest.TestCase):
    def test_the_chosen_compiler_is_the_one_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "calls")
            stub = os.path.join(tmp, "wrela")
            with open(stub, "w") as f:
                f.write(f'#!/bin/sh\necho "$@" >> {log}\necho \'{{"version": 2, "diagnostics": []}}\'\n')
            os.chmod(stub, 0o755)
            task = os.path.join(tmp, "task")
            os.makedirs(os.path.join(task, "pkg"))
            with open(os.path.join(task, "pkg", "main.wrela"), "w") as f:
                f.write("pub fn frame(time: f32, width: u32, height: u32) {}\n")
            env = dict(os.environ, WRELA_BIN=stub, PATH="/usr/bin:/bin")
            out = subprocess.run([os.path.join(V2, "attempt.sh"), task], env=env, capture_output=True, text=True)
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            with open(log) as f:
                calls = f.read().splitlines()
            self.assertTrue(calls and all(c.startswith("build ") for c in calls), calls)
            with open(os.path.join(task, "attempts", "1", "status")) as f:
                self.assertEqual(f.read().strip(), "0")
            self.assertTrue(os.path.isfile(os.path.join(task, "attempts", "1", "src", "main.wrela")))
            # Without a compiler it refuses, and builds none (cargo isn't on that PATH either).
            env.pop("WRELA_BIN")
            out = subprocess.run([os.path.join(V2, "attempt.sh"), task], env=env, capture_output=True, text=True)
            self.assertEqual(out.returncode, 2, out.stderr)
            # A dry run prints each agent's command with this compiler, and launches nothing.
            out = subprocess.run([sys.executable, os.path.join(V2, "run.py"), "dry-run-test", "--wrela", stub,
                                  "--tasks", "01-area,13-arrays", "--dry-run"], capture_output=True, text=True)
            self.assertEqual(out.returncode, 0, out.stderr)
            lines = out.stdout.strip().splitlines()
            self.assertEqual(len(lines), 3, out.stdout)
            self.assertTrue(all(l.startswith("claude -p") and f"Bash({stub} doc:*)" in l for l in lines[:2]), lines)
            self.assertFalse(os.path.exists(os.path.join(ROOT, "target", "agent-test", "dry-run-test")))


def write(path, text):
    with open(path, "w") as f:
        f.write(text)


class Score(unittest.TestCase):
    def run_dir(self, tmp, attempts):
        """A run of the first task whose attempts are (status, correct) pairs."""
        first = grading.tasks()[0]["id"]
        for n, (status, correct) in enumerate(attempts, 1):
            a = os.path.join(tmp, first, "attempts", str(n))
            os.makedirs(a)
            write(os.path.join(a, "status"), status)
            write(os.path.join(a, "diagnostics.json"), json.dumps({"diagnostics": []}))
            graded = {"correct": correct, "why": "" if correct else "tests failed: grade_x"}
            write(os.path.join(a, "graded.json"), json.dumps(graded))
        usage = {"usage": {"input_tokens": 10, "output_tokens": 5}, "total_cost_usd": 0.5}
        write(os.path.join(tmp, first, "usage.json"), json.dumps(usage))
        out = subprocess.run([sys.executable, os.path.join(V2, "score.py"), tmp],
                             capture_output=True, text=True, check=True).stdout
        return json.loads(out)

    def test_a_build_that_is_wrong_then_a_right_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            r = self.run_dir(tmp, [("0", False), ("0", True)])
            t = r["per_task"][0]
            self.assertEqual((t["built_at"], t["correct_at"]), (1, 2))
            self.assertEqual(r["first_try_correct"], 0)
            self.assertEqual(r["first_try_builds"], 1)
            self.assertEqual(t["tokens"], 15)
            self.assertEqual(t["graded"][0], "tests failed: grade_x")

    def test_never_correct(self):
        with tempfile.TemporaryDirectory() as tmp:
            r = self.run_dir(tmp, [("1", False), ("0", False)])
            self.assertIsNone(r["per_task"][0]["correct_at"])
            self.assertEqual(r["correct_eventually"], 0)


if __name__ == "__main__":
    unittest.main()
