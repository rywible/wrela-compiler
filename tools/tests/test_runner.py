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
import memo
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


class MemoWorkers(unittest.TestCase):
    def test_all_tests_remembered_by_their_builds_still_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / "tests.json").write_text(json.dumps({
                "reason": "compiler-artifact", "profile": {"test": True},
                "executable": str(root / "test-program"), "manifest_path": str(root / "Cargo.toml"),
                "target": {"name": "suite", "kind": ["test"], "src_path": str(root / "suite.rs")},
            }))
            store = types.SimpleNamespace(timing=lambda name: 6, prune=lambda: None)
            fake = types.SimpleNamespace(store=store, remembered=lambda *a, builds=True: builds,
                                         builds_wanted=lambda *a: [],
                                         rebuilds=types.SimpleNamespace(prefetch=lambda builds: None))

            def listed(command, **kwargs):
                return types.SimpleNamespace(stdout="" if "--skip" in command else "slow: test\n")

            with patch.object(memo, "Memo", return_value=fake), \
                 patch.object(memo.subprocess, "run", side_effect=listed), \
                 patch.object(runner, "scratch_root", return_value=str(root)), \
                 patch.dict(os.environ, {"CARGO_TARGET_DIR": str(root / "target")}), \
                 contextlib.redirect_stdout(io.StringIO()):
                status = memo.run_tests(str(root), str(root / "cache"), "on", str(root / "store"),
                                        [], os.getppid())
            self.assertEqual(status, 0)

    def test_quick_and_slow_tests_share_the_available_cpu_budget(self):
        for cores in (1, 2, 4):
            with self.subTest(cores=cores), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                logs = root / "logs"
                logs.mkdir()
                counter = root / "counter.json"
                counter.write_text(json.dumps({"active": 0, "peak": 0, "ran": 0}))
                script = root / "fake-tests"
                script.write_text(f"#!{sys.executable}\n" + textwrap.dedent(f"""
                    import fcntl, json, pathlib, sys, time
                    if '--list' in sys.argv:
                        print('normal: test')
                        if '--skip' not in sys.argv: print('slow: test')
                        sys.exit(0)
                    def count(change):
                        with open({str(counter)!r}, 'r+') as f:
                            fcntl.flock(f, fcntl.LOCK_EX)
                            data = json.load(f)
                            data['active'] += change
                            data['peak'] = max(data['peak'], data['active'])
                            if change < 0: data['ran'] += 1
                            f.seek(0); json.dump(data, f); f.truncate()
                    count(1)
                    time.sleep(0.03)
                    count(-1)
                    if '--exact' in sys.argv: print('test result: ok. 1 passed')
                    else: print(json.dumps({{'type': 'test', 'event': 'ok', 'name': 'normal'}}))
                """))
                script.chmod(0o755)
                programs = [{
                    "reason": "compiler-artifact", "profile": {"test": True},
                    "executable": str(script), "manifest_path": str(root / "Cargo.toml"),
                    "target": {"name": f"suite-{i}", "kind": ["test"], "src_path": str(root / "suite.rs")},
                } for i in range(3)]
                (logs / "tests.json").write_text("\n".join(json.dumps(p) for p in programs))
                store = types.SimpleNamespace(timing=lambda name: 6 if name.endswith(":slow") else 0,
                                              set_timing=lambda *a, **k: None, prune=lambda: None)
                fake = types.SimpleNamespace(store=store, remember=lambda *a: "crates")

                def isolate(exe, name, scratch, tmp, trace):
                    pathlib.Path(scratch).mkdir(parents=True)
                    pathlib.Path(trace).write_text("")
                    return [exe, "--exact", name]

                with patch.object(memo, "Memo", return_value=fake), \
                     patch.object(resources, "cpu_count", return_value=cores), \
                     patch.object(memo, "isolated_command", side_effect=isolate), \
                     patch.object(runner, "scratch_root", return_value=str(root)), \
                     patch.dict(os.environ, {"CARGO_TARGET_DIR": str(root / "target")}), \
                     contextlib.redirect_stdout(io.StringIO()):
                    status = memo.run_tests(str(logs), str(root / "cache"), "fresh", str(root / "store"),
                                            [], os.getppid())
                counts = json.loads(counter.read_text())
                self.assertEqual(status, 0)
                self.assertEqual(counts["ran"], 6)
                self.assertEqual(counts["active"], 0)
                self.assertLessEqual(counts["peak"], cores)

    def test_rebuild_prefetch_respects_cpu_limits_and_deduplicates_work(self):
        rebuilds = memo.Rebuilds("unused", "/", "/tmp/unused")
        with patch.object(resources, "cpu_count", return_value=3), \
             patch("concurrent.futures.ThreadPoolExecutor") as pool, \
             patch.object(rebuilds, "make", return_value="hash"):
            pool.return_value.__enter__.return_value.map.side_effect = lambda f, items: map(f, items)
            rebuilds.prefetch([("a", []), ("a", []), ("b", ["--testing"])])
        pool.assert_called_once_with(max_workers=3)
        self.assertEqual(rebuilds.known, {("a", ()): "hash", ("b", ("--testing",)): "hash"})


if __name__ == "__main__":
    unittest.main()
