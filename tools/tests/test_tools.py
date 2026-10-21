"""Tests for tools/agent-test's scoring (score.py).

    python3 -m unittest discover -s tools/tests
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

TESTS = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.dirname(TESTS)


class ScoreTest(unittest.TestCase):
    def test_partial_attempts_and_stray_files(self):
        with open(os.path.join(TOOLS, "agent-test", "tasks.json")) as f:
            first = json.load(f)[0]["id"]
        with tempfile.TemporaryDirectory() as run:
            attempts = os.path.join(run, first, "attempts")

            def write(n, name, text):
                os.makedirs(os.path.join(attempts, n), exist_ok=True)
                with open(os.path.join(attempts, n, name), "w") as f:
                    f.write(text)

            diags = {"diagnostics": [{"code": "E0301", "severity": "error"}]}
            write("1", "status", "1\n")
            write("1", "diagnostics.json", json.dumps(diags))
            write("2", "diagnostics.json", "")  # stopped while the compiler ran
            write("3", "status", "0\n")
            write("3", "diagnostics.json", '{"diagnostics": []}')
            with open(os.path.join(attempts, ".DS_Store"), "w") as f:
                f.write("Finder")
            out = subprocess.run(
                [sys.executable, os.path.join(TOOLS, "agent-test", "score.py"), run],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
        report = json.loads(out)
        task = next(p for p in report["per_task"] if p["task"] == first)
        self.assertEqual(task["attempts"], 3)
        self.assertEqual(task["built_at"], 3)
        self.assertEqual(task["unfinished"], [2])
        self.assertEqual(task["codes_per_attempt"], [["E0301"], [], []])
        self.assertEqual(task["over_limit"], [])
        self.assertEqual(report["built_eventually"], 1)

    def test_attempts_past_the_limit_dont_count(self):
        with open(os.path.join(TOOLS, "agent-test", "tasks.json")) as f:
            first = json.load(f)[0]["id"]
        with open(os.path.join(TOOLS, "agent-test", "config.json")) as f:
            limit = json.load(f)["max_attempts"]
        with tempfile.TemporaryDirectory() as run:
            for n in range(1, limit + 2):
                at = os.path.join(run, first, "attempts", str(n))
                os.makedirs(at)
                with open(os.path.join(at, "status"), "w") as f:
                    f.write("0\n" if n == limit + 1 else "1\n")
            out = subprocess.run(
                [sys.executable, os.path.join(TOOLS, "agent-test", "score.py"), run],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
        task = next(p for p in json.loads(out)["per_task"] if p["task"] == first)
        self.assertEqual(task["attempts"], limit)
        self.assertIsNone(task["built_at"])
        self.assertEqual(task["over_limit"], [limit + 1])


if __name__ == "__main__":
    unittest.main()
