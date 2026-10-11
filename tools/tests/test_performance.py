"""Regression checks for the tools' caches, size budgets and round reports."""

import concurrent.futures
import contextlib
import gzip
import io
import json
import os
import pathlib
import random
import runpy
import sys
import tempfile
import threading
import unittest
from unittest import mock

TOOLS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, TOOLS)

import round as rounds  # noqa: E402
import serve  # noqa: E402
import subjects  # noqa: E402


class GzipCacheTest(unittest.TestCase):
    def test_rebuilds_replace_the_cached_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "game.wasm")
            original = gzip.compress
            with mock.patch.object(serve.gzip, "compress", wraps=original) as compress:
                for i, version in enumerate((b"first", b"other", b"new build")):
                    pathlib.Path(path).write_bytes(version)
                    stamp = 1_000_000_000 + i
                    os.utime(path, ns=(stamp, stamp))
                    body = serve.gzipped(path)
                    self.assertEqual(gzip.decompress(body), version)
                    self.assertIs(serve.gzipped(path), body)
                self.assertEqual(compress.call_count, 3)
                self.assertEqual(serve._GZIPPED[path].body, body)
            with serve._GZIPPED_LOCK:
                del serve._GZIPPED[path]

    def test_simultaneous_requests_compress_a_version_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "game.wasm")
            pathlib.Path(path).write_bytes(b"wasm" * 100)
            started, release = threading.Event(), threading.Event()
            original = gzip.compress

            def compress(data, **kwargs):
                started.set()
                self.assertTrue(release.wait(5))
                return original(data, **kwargs)

            with mock.patch.object(serve.gzip, "compress", side_effect=compress) as packed:
                with concurrent.futures.ThreadPoolExecutor(8) as pool:
                    pending = [pool.submit(serve.gzipped, path) for _ in range(8)]
                    self.assertTrue(started.wait(5))
                    release.set()
                    bodies = [p.result(5) for p in pending]
                self.assertEqual(packed.call_count, 1)
                self.assertTrue(all(body is bodies[0] for body in bodies))
            with serve._GZIPPED_LOCK:
                del serve._GZIPPED[path]


class RoundSummaryTest(unittest.TestCase):
    def test_nearest_report_keeps_original_ties_and_duplicate_frames(self):
        frames = [8, 2, 8, 14]
        names = ["first", "earlier", "duplicate", "last"]
        reports = [json.dumps({"report": True, "region": name, "site_m": 10,
                              "site": name, "at": [0, 0], "water": 0})
                   for name in names]
        # Every interval is a hitch, so this covers both endpoints, exact hits and midpoints.
        timing = {"began_ms": [40 * i for i in range(18)]}
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()):
            summary = rounds.summarize(tmp, timing, reports, frames, snaps={3})
        self.assertNotIn(4, [h["frame"] for h in summary["hitches"]])
        for hitch in summary["hitches"]:
            frame = hitch["frame"]
            nearest = min(range(len(frames)), key=lambda i: abs(frames[i] - frame))
            self.assertTrue(hitch["where"].startswith(names[nearest] + ","), hitch)
        self.assertEqual(summary["regions"], names)
        self.assertEqual(summary["reports"], len(reports))

    def test_missing_reports_and_frames_are_empty(self):
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()):
            summary = rounds.summarize(tmp, {"began_ms": []}, [], [])
        self.assertEqual((summary["hitches"], summary["reports"]), ([], 0))


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
            self.run_budgets({"examples/hello-field": b"hello", "examples/other": b"other"})
        self.assertEqual(compress.call_count, 1)
        self.assertEqual(compress.call_args.args[0], b"hello")

    def test_compressed_and_raw_budget_failures_keep_the_sizes(self):
        cases = [("examples/hello-field", random.Random(0).randbytes(30 * 1024)),
                 ("examples/other", b"x" * (256 * 1024 + 1))]
        for package, shader in cases:
            with self.subTest(package=package), self.assertRaises(SystemExit) as error:
                self.run_budgets({package: shader})
            self.assertIn(package + "/pipeline_0.wgsl", str(error.exception))
            self.assertIn("bytes over", str(error.exception))


if __name__ == "__main__":
    unittest.main()
