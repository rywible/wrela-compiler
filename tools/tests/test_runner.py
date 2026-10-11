"""The CPU budget applies to quick and isolated tests together, and worker failures propagate."""

import contextlib
import io
import json
import os
import pathlib
import sys
import tempfile
import textwrap
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
import resources
import runner


class ToolWorkers(unittest.TestCase):
    def test_reused_workers_run_every_fixture_and_report_failures(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            record = directory / "ran"
            source = directory / "test_worker_fixture.py"
            source.write_text(textwrap.dedent(f"""
                import os, pathlib, unittest
                class Fixture(unittest.TestCase):
                    def setUp(self):
                        with pathlib.Path({str(record)!r}).open('a') as out:
                            out.write(str(os.getpid()) + '\\n')
                    def test_a(self): pass
                    def test_b(self): pass
                    def test_c(self): pass
                    def test_d(self): self.fail('worker failure is visible')
            """))
            output = io.StringIO()
            with patch.object(resources, "cpu_count", return_value=1), contextlib.redirect_stdout(output):
                status = runner.tools_tests(str(directory))
            self.assertEqual(status, 1)
            self.assertIn("worker failure is visible", output.getvalue())
            self.assertIn("4 tools tests, 1 failed", output.getvalue())
            workers = record.read_text().splitlines()
            self.assertEqual(len(workers), 4)
            self.assertLessEqual(len(set(workers)), 2)


if __name__ == "__main__":
    unittest.main()
