"""Tool discovery runs every fixture and reports failures."""

import contextlib
import io
import os
import pathlib
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
import runner


class ToolDiscovery(unittest.TestCase):
    def test_every_fixture_runs_and_failures_are_reported(self):
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
            with contextlib.redirect_stderr(output):
                status = runner.tools_tests(str(directory))
            self.assertEqual(status, 1)
            self.assertIn("worker failure is visible", output.getvalue())
            self.assertIn("Ran 4 tests", output.getvalue())
            workers = record.read_text().splitlines()
            self.assertEqual(len(workers), 4)
            self.assertEqual(set(workers), {str(os.getpid())})

    def test_skips_and_empty_discovery_are_failures(self):
        with tempfile.TemporaryDirectory() as temporary, contextlib.redirect_stderr(io.StringIO()):
            directory = pathlib.Path(temporary)
            self.assertEqual(runner.tools_tests(str(directory)), 1)
            (directory / "test_skip_fixture.py").write_text(
                "import unittest\nclass Fixture(unittest.TestCase):\n"
                "    @unittest.skip('fixture skip')\n    def test_skipped(self): pass\n")
            self.assertEqual(runner.tools_tests(str(directory)), 1)

    def test_selected_test_restores_the_callers_directory(self):
        caller = pathlib.Path.cwd()
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            (directory / "test_selected_fixture.py").write_text(
                "import unittest\nclass Fixture(unittest.TestCase):\n"
                "    def test_failure(self): self.fail('visible failure')\n")
            passed, output = runner._tool_test("test_selected_fixture.Fixture.test_failure", str(directory))
            self.assertFalse(passed)
            self.assertIn("visible failure", output)
            self.assertEqual(pathlib.Path.cwd(), caller)


if __name__ == "__main__":
    unittest.main()
