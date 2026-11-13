"""Tests for the agent test, version 3 (tools/agent-test/v3): every task's grade passes its right
reference and fails its wrong one; the dry run prints both arms' commands and runs nothing; and
the score reads a run's arms side by side.

    python3 -m unittest discover -s tools/tests

The references are graded with the compiler at `WRELA_BIN` (an absolute path; `tools/check.sh`
gives the release compiler it builds); without it, this checkout's release compiler, built
first. The pixel and make-it-cheaper grades run frames on this machine's GPU.
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
V3 = os.path.join(ROOT, "tools", "agent-test", "v3")
sys.path.insert(0, V3)
import grading  # noqa: E402
import score  # noqa: E402

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

    def one(t):
        return t["id"], grading.grade(t, grading.reference(t, side), w)
    # GPU frames and fuzzing run a few at a time.
    with ThreadPoolExecutor(4) as ex:
        return list(ex.map(one, grading.tasks()))


class References(unittest.TestCase):
    def test_every_task_has_its_start_grade_and_references(self):
        tasks = grading.tasks()
        self.assertEqual(len(tasks), 20)
        kinds = {}
        for t in tasks:
            kinds[t["kind"]] = kinds.get(t["kind"], 0) + 1
            for side in ("right", "wrong"):
                self.assertTrue(os.path.isfile(os.path.join(grading.reference(t, side), "main.wrela")), t["id"])
            self.assertTrue(os.path.isfile(os.path.join(grading.start_of(t), "main.wrela")), t["id"])
            if t["grade"].get("tests"):
                self.assertTrue(os.path.isfile(os.path.join(V3, "grades", t["id"] + ".wrela")), t["id"])
        self.assertEqual(kinds, {"shape": 5, "repair": 4, "pixel": 4, "trap": 3, "cheaper": 4})

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
    def test_the_dry_run_prints_both_arms_and_runs_nothing(self):
        name = "v3-dry-run-test"
        out = subprocess.run(
            [sys.executable, os.path.join(V3, "run.py"), name, "--wrela", wrela(), "--dry-run"],
            capture_output=True, text=True, check=True,
        ).stdout
        lines = out.strip().split("\n")
        self.assertEqual(sum(1 for l in lines if l.startswith("[with] claude -p")), 20)
        self.assertEqual(sum(1 for l in lines if l.startswith("[without] claude -p")), 20)
        with open(os.path.join(V3, "config.json")) as f:
            cfg = json.load(f)
        self.assertIn(f"40 agents; at most ${40 * cfg['budget_usd']}", lines[-1])
        withs = [l for l in lines if l.startswith("[with]")]
        withouts = [l for l in lines if l.startswith("[without]")]
        self.assertTrue(all(" inspect:*)" in l and " pixel:*)" in l and " fuzz:*)" in l for l in withs))
        self.assertTrue(all(" inspect:" not in l and " pixel:" not in l for l in withouts))
        self.assertFalse(os.path.exists(os.path.join(ROOT, "target", "agent-test", name)))


def write(path, text):
    with open(path, "w") as f:
        f.write(text)


class Score(unittest.TestCase):
    def test_the_arms_are_scored_side_by_side(self):
        with tempfile.TemporaryDirectory() as run:
            ids = ["s1-stool", "p1-nan"]
            write(os.path.join(run, "run.json"), json.dumps({"arms": ["with", "without"], "tasks": ids}))

            def task(arm, id, correct_at, attempts, cmds):
                d = os.path.join(run, arm, id)
                for n in range(1, attempts + 1):
                    a = os.path.join(d, "attempts", str(n))
                    os.makedirs(a)
                    write(os.path.join(a, "graded.json"), json.dumps({"correct": n == correct_at}))
                calls = [{"type": "tool_use", "name": "Bash", "input": {"command": c}} for c in cmds]
                lines = [json.dumps({"type": "assistant", "message": {"content": calls}}),
                         json.dumps({"type": "result", "result": "done", "total_cost_usd": 0.5,
                                     "usage": {"input_tokens": 100, "output_tokens": 20}})]
                write(os.path.join(d, "transcript.jsonl"), "\n".join(lines) + "\n")

            task("with", "s1-stool", 1, 1, ["/r/attempt /t", "/bin/wrela inspect /p shape::subject"])
            task("with", "p1-nan", 2, 2, ["/bin/wrela pixel /p --frame 1 --at 1,1"])
            task("without", "s1-stool", None, 3, ["/r/attempt /t"])
            task("without", "p1-nan", 1, 1, [])
            r = score.report(run)
            self.assertEqual((r["with"]["correct"], r["without"]["correct"]), (2, 1))
            self.assertEqual(r["with"]["mean_attempts_to_correct"], 1.5)
            self.assertEqual(r["with"]["commands"], {"attempt": 1, "inspect": 1, "pixel": 1})
            self.assertEqual(r["with"]["mean_tokens"], 120)
            self.assertEqual(r["without"]["correct_by_kind"], {"shape": 0, "pixel": 1})


if __name__ == "__main__":
    unittest.main()
