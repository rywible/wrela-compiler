"""Exercise focused checks without compiling Rust or running the real gate."""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


TOOLS = pathlib.Path(__file__).resolve().parents[1]
CHECK = TOOLS / "check.sh"


class FocusedCheck(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        target = tempfile.TemporaryDirectory()
        self.addCleanup(target.cleanup)
        self.target = pathlib.Path(target.name)
        for directory in ("tools", "bin", "compiler/tests", "runtime", "tmp"):
            (self.root / directory).mkdir(parents=True, exist_ok=True)
        shutil.copyfile(CHECK, self.root / "tools/check.sh")
        (self.root / "tools/resources.py").write_text(textwrap.dedent("""
            import errno, os
            if os.environ.get("FAKE_LINK_ERROR"):
                def unavailable_link(*args, **kwargs):
                    raise OSError(getattr(errno, os.environ["FAKE_LINK_ERROR"]), "fake link failure")
                os.link = unavailable_link
            def cpu_count():
                return int(os.environ.get("FAKE_CPUS", "2"))
            if __name__ == "__main__":
                print(cpu_count())
        """))
        (self.root / "compiler/tests/cases.rs").write_text(
            '#[ignore = "long: extended check"]\nfn long_case() {}\n'
            '#[ignore = "measure: timing"]\nfn measure_case() {}\n'
        )
        self.script("bin/cargo", """
            import json, os, pathlib, sys
            root = pathlib.Path(os.environ["FAKE_ROOT"])
            with (root / "cargo.jsonl").open("a") as output:
                output.write(json.dumps(sys.argv[1:]) + "\\n")
            if sys.argv[1] != "test" or "--no-run" not in sys.argv:
                sys.exit(97)
            print(json.dumps({"reason": "compiler-artifact", "profile": {"test": True},
                "executable": str(root / "fake-tests"),
                "manifest_path": str(root / "compiler/tests/Cargo.toml"),
                "target": {"name": "fake-tests", "kind": ["test"]}}))
        """)
        self.script("fake-tests", """
            import json, os, pathlib, subprocess, sys
            if "--worker" in sys.argv:
                print("reexecuted original test program")
                sys.exit(0)
            args = sys.argv[1:]
            skips, filters = [], []
            remaining = iter(args)
            for arg in remaining:
                if arg == "--skip":
                    skips.append(next(remaining))
                elif not arg.startswith("-"):
                    filters.append(arg)
            selected = [name for name in ["normal_case", "long_case", "measure_case"]
                        if not any(skip in name for skip in skips)
                        and (not filters or any(f in name for f in filters))
                        and (name == "normal_case" or "--include-ignored" in args)]
            root = pathlib.Path(os.environ["FAKE_ROOT"])
            (root / "test.json").write_text(json.dumps({"args": args, "selected": selected,
                "scratch": os.environ["WRELA_TEST_TMPDIR"],
                "cache": os.environ["WRELA_NATIVE_CACHE"],
                "const_cache": os.environ.get("WRELA_CONST_CACHE"),
                "threads": os.environ["RUST_TEST_THREADS"]}))
            if os.environ.get("FAKE_REPLACE_EXECUTABLE") == "1":
                original = root / "fake-tests"
                original.unlink()
                original.write_text("#!/bin/sh\\nexit 99\\n")
                original.chmod(0o755)
                subprocess.run([sys.argv[0], "--worker"], check=True)
            print("fake test output")
            if os.environ.get("FAKE_TEST_FAIL") == "1":
                print("test result: FAILED. 0 passed; 1 failed")
                sys.exit(1)
            print(f"test result: ok. {len(selected)} passed; 0 failed")
        """)

    def script(self, name, source):
        path = self.root / name
        path.write_text(f"#!{sys.executable}\n" + textwrap.dedent(source))
        path.chmod(0o755)

    def check(self, *args, fail=False, cores=2, software=None, replace_executable=False, link_error=""):
        for name in ("cargo.jsonl", "test.json"):
            (self.root / name).unlink(missing_ok=True)
        env = {**os.environ, "PATH": f"{self.root / 'bin'}:{os.environ['PATH']}",
               "FAKE_ROOT": str(self.root), "FAKE_TEST_FAIL": "1" if fail else "0",
               "FAKE_REPLACE_EXECUTABLE": "1" if replace_executable else "0",
               "FAKE_LINK_ERROR": link_error,
               "FAKE_CPUS": str(cores), "TMPDIR": str(self.root / "tmp"),
               "WRELA_CONST_CACHE": str(self.target / "stale-const-cache"),
               "CARGO_TARGET_DIR": str(self.target)}
        env.pop("WRELA_SOFTWARE_GPU", None)
        if software is not None:
            env["WRELA_SOFTWARE_GPU"] = software
        return subprocess.run(
            ["bash", str(self.root / "tools/check.sh"), *args],
            cwd=self.root / "tmp", capture_output=True, text=True, timeout=10,
            env=env,
        )

    def recorded_test(self):
        return json.loads((self.root / "test.json").read_text())

    def assert_focused_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("focused check", result.stdout.lower())
        self.assertNotIn("all checks passed", result.stdout.lower())

    def test_packages_limit_build_and_accept_nocapture_without_a_filter(self):
        result = self.check("--package=wrela-syntax", "--package=wrela-abi", "--nocapture")
        self.assert_focused_success(result)
        calls = [json.loads(line) for line in (self.root / "cargo.jsonl").read_text().splitlines()]
        self.assertEqual(len(calls), 1)
        self.assertNotIn("--workspace", calls[0])
        packages = []
        remaining = iter(calls[0])
        for arg in remaining:
            if arg in ("--package", "-p"):
                packages.append(next(remaining))
            elif arg.startswith("--package="):
                packages.append(arg.split("=", 1)[1])
        self.assertEqual(packages, ["wrela-syntax", "wrela-abi"])
        recorded = self.recorded_test()
        self.assertIn("--nocapture", recorded["args"])
        self.assertEqual(recorded["selected"], ["normal_case"])
        self.assertEqual(pathlib.Path(recorded["cache"]), self.target / "native-cache")
        scratch = pathlib.Path(recorded["scratch"])
        self.assertEqual(scratch.parent, self.root / "target")
        self.assertFalse(scratch.exists())
        self.assertIn("fake test output", result.stdout)

    def test_package_filter_and_nocapture_reach_the_test_program(self):
        for prefix in (("--package=wrela-syntax",), ()):
            with self.subTest(prefix=prefix):
                result = self.check(*prefix, "normal_case", "--nocapture")
                self.assert_focused_success(result)
                calls = [json.loads(line) for line in (self.root / "cargo.jsonl").read_text().splitlines()]
                self.assertEqual(len(calls), 1)
                self.assertEqual("--workspace" in calls[0], not prefix)
                self.assertIn("normal_case", self.recorded_test()["args"])
                self.assertIn("--nocapture", self.recorded_test()["args"])
                self.assertEqual(self.recorded_test()["selected"], ["normal_case"])
                self.assertIn("fake test output", result.stdout)

    def test_long_includes_long_cases_but_never_measurements(self):
        result = self.check("--package=wrela-syntax", "--long")
        self.assert_focused_success(result)
        self.assertEqual(self.recorded_test()["selected"], ["normal_case", "long_case"])

    def test_heavy_cases_run_on_hardware_and_in_the_software_long_tier(self):
        source = self.root / "compiler/tests/cases.rs"
        source.write_text(source.read_text() + '#[ignore = "heavy: rendering"]\nfn heavy_case() {}\n')
        program = self.root / "fake-tests"
        program.write_text(program.read_text().replace(
            '"long_case", "measure_case"', '"long_case", "measure_case", "heavy_case"'))
        for software, flags, selected in (
            (None, (), ["normal_case", "heavy_case"]),
            ("1", (), ["normal_case"]),
            ("1", ("--long",), ["normal_case", "long_case", "heavy_case"]),
        ):
            with self.subTest(software=software, flags=flags):
                result = self.check("--package=wrela-syntax", *flags, software=software)
                self.assert_focused_success(result)
                self.assertEqual(self.recorded_test()["selected"], selected)
                self.assertIsNone(self.recorded_test()["const_cache"])

    def test_scratch_keeps_copied_fixtures_relative_dependencies_valid(self):
        result = self.check("--package=wrela-syntax")
        self.assert_focused_success(result)
        scratch = pathlib.Path(self.recorded_test()["scratch"])
        fixture = scratch / "diagnostics" / "a-case"
        self.assertEqual((fixture / "../../../../engine").resolve(), self.root / "engine")

    def test_workers_reexecute_the_original_program_after_cargo_replaces_it(self):
        original = self.root / "fake-tests"
        source = original.read_bytes()
        for link_error in ("", "EXDEV"):
            with self.subTest(link_error=link_error):
                original.write_bytes(source)
                result = self.check("--package=wrela-syntax", "--nocapture",
                                    replace_executable=True, link_error=link_error)
                self.assert_focused_success(result)
                self.assertNotEqual(original.read_bytes(), source)
                self.assertIn("reexecuted original test program", result.stdout)

    def test_executable_link_permission_errors_fail_the_check(self):
        result = self.check("--package=wrela-syntax", link_error="EACCES")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("fake link failure", result.stdout + result.stderr)
        self.assertFalse((self.root / "test.json").exists())

    def test_threads_follow_cpu_and_software_gpu_limits(self):
        for cores, software, threads in ((1, None, 1), (2, None, 2), (16, None, 4),
                                         (16, "1", 1), (16, "0", 1), (16, "", 4)):
            with self.subTest(cores=cores, software=software):
                result = self.check("--package=wrela-syntax", cores=cores, software=software)
                self.assert_focused_success(result)
                self.assertEqual(self.recorded_test()["threads"], str(threads))

    def test_invalid_modes_are_rejected_before_cargo_runs(self):
        for args in (("--package=",), *(("--package=wrela-syntax", mode)
                                       for mode in ("--fresh", "--full", "--fmt"))):
            with self.subTest(args=args):
                result = self.check(*args)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertFalse((self.root / "cargo.jsonl").exists())

    def test_failed_tests_and_unmatched_filters_do_not_pass(self):
        for filter_name, fail in (("normal_case", True), ("missing_case", False)):
            with self.subTest(filter=filter_name, fail=fail):
                result = self.check("--package=wrela-syntax", filter_name, fail=fail)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue((self.root / "test.json").exists())
                self.assertNotIn("all checks passed", result.stdout.lower())
                scratch = pathlib.Path(self.recorded_test()["scratch"])
                self.assertEqual(scratch.parent, self.root / "target")
                self.assertEqual(scratch.exists(), fail)


if __name__ == "__main__":
    unittest.main()
