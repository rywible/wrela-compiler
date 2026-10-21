"""Regression checks for the WGSL size budgets (tools/wgsl_budgets.py)."""

import contextlib
import gzip
import io
import os
import pathlib
import random
import sys
import unittest
from unittest import mock

TOOLS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, TOOLS)


class WgslBudgetTest(unittest.TestCase):
    def run_budgets(self, shaders):
        import subprocess
        import wgsl_budgets
        def build(args, **kwargs):
            built = pathlib.Path(args[args.index("-o") + 1])
            built.mkdir(parents=True)
            (built / "pipeline_0.wgsl").write_bytes(shaders[args[2]])
            return subprocess.CompletedProcess(args, 0, "", "")
        with mock.patch.object(wgsl_budgets, "PACKAGES", tuple(shaders)), \
                mock.patch("subprocess.run", side_effect=build), \
                contextlib.redirect_stdout(io.StringIO()):
            wgsl_budgets.main("fake-wrela")

    def test_only_compressed_budgets_compress_successful_shaders(self):
        original = gzip.compress
        with mock.patch("gzip.compress", wraps=original) as compress:
            import wgsl_budgets
            self.run_budgets({wgsl_budgets.HELLO: b"hello", "examples/other": b"other"})
        self.assertEqual(compress.call_count, 1)
        self.assertEqual(compress.call_args.args[0], b"hello")

    def test_compressed_and_raw_budget_failures_keep_the_sizes(self):
        import wgsl_budgets
        cases = [(wgsl_budgets.HELLO, random.Random(0).randbytes(30 * 1024)),
                 ("examples/other", b"x" * (256 * 1024 + 1))]
        for package, shader in cases:
            with self.subTest(package=package), self.assertRaises(SystemExit) as error:
                self.run_budgets({package: shader})
            self.assertIn(package + "/pipeline_0.wgsl", str(error.exception))
            self.assertIn("bytes over", str(error.exception))


if __name__ == "__main__":
    unittest.main()
