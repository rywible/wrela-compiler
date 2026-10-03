"""Tests for tools/headless.py and tools/serve.py, with a fake Chrome (no browser, no GPU).

    python3 -m unittest discover -s tools/tests
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request

TESTS = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.dirname(TESTS)
ROOT = os.path.dirname(TOOLS)
sys.path.insert(0, TOOLS)

import headless  # noqa: E402
import serve  # noqa: E402


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


class PageDir:
    """Mixin: each test gets a page directory in the repo, `page_dir` (`page` relative to the
    repo root), removed afterwards."""

    def setUp(self):
        super().setUp()
        self.page_dir = tempfile.mkdtemp(prefix=".test-page-", dir=TOOLS)
        self.page = os.path.relpath(self.page_dir, ROOT)

    def tearDown(self):
        shutil.rmtree(self.page_dir, ignore_errors=True)
        super().tearDown()


class HeadlessTest(PageDir, unittest.TestCase):
    def setUp(self):
        super().setUp()
        self.tmp = tempfile.mkdtemp()  # the lock and the fake's notes; TMPDIR isolates the lock
        self.child_pid_file = os.path.join(self.tmp, "child.pid")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)
        super().tearDown()

    def start(self, mode, timeout="20", **env):
        env = {
            **os.environ,
            "TMPDIR": self.tmp,
            "WRELA_CHROME": os.path.join(TESTS, "fake_chrome.py"),
            "FAKE_MODE": mode,
            "FAKE_CHILD_PID": self.child_pid_file,
            **env,
        }
        return subprocess.Popen(
            [sys.executable, os.path.join(TOOLS, "headless.py"), self.page, "#run", timeout],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

    def run_headless(self, mode, timeout="20", **env):
        proc = self.start(mode, timeout, **env)
        out, err = proc.communicate(timeout=60)
        return proc.returncode, out, err

    def chrome_pids(self):
        """(fake Chrome, the child it started), once it has started."""
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                with open(self.child_pid_file) as f:
                    pids = f.read().split()
                if len(pids) == 2:
                    return tuple(map(int, pids))
            except FileNotFoundError:
                pass
            time.sleep(0.05)
        self.fail("the fake Chrome never started")

    def assert_chrome_gone(self):
        chrome, child = self.chrome_pids()
        deadline = time.time() + 5
        while (alive(chrome) or alive(child)) and time.time() < deadline:
            time.sleep(0.05)
        self.assertFalse(alive(child), "a process Chrome started outlived the run")

    def test_ok_passes_and_keeps_the_console(self):
        status, out, err = self.run_headless("ok")
        self.assertEqual(status, 0, err)
        with open(os.path.join(self.page_dir, "results", "console.log")) as f:
            self.assertEqual(
                f.read(),
                "hello from the page\npipeline `p` failed to build:\n3:5: unresolved identifier\n",
            )
        self.assertIn("hello from the page", out)
        self.assert_chrome_gone()

    def test_any_other_done_body_fails_with_it(self):
        status, _, err = self.run_headless("fail")
        self.assertEqual(status, 1)
        self.assertIn("page failed: WGSL compile error", err)
        self.assert_chrome_gone()

    def test_a_stale_done_doesnt_pass_a_run(self):
        os.makedirs(os.path.join(self.page_dir, "results"))
        with open(os.path.join(self.page_dir, "results", "DONE"), "w") as f:
            f.write("ok")
        status, _, err = self.run_headless("hang", timeout="1")
        self.assertEqual(status, 1)
        self.assertIn("timed out after 1s", err)
        self.assert_chrome_gone()

    def test_chrome_exiting_early_fails(self):
        status, _, err = self.run_headless("exit")
        self.assertEqual(status, 1)
        self.assertIn("chrome exited early (status 4)", err)
        self.assert_chrome_gone()

    def test_sigterm_stops_chrome(self):
        proc = self.start("hang")
        self.chrome_pids()
        proc.send_signal(signal.SIGTERM)
        _, err = proc.communicate(timeout=10)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("interrupted", err)
        self.assert_chrome_gone()

    def test_runs_take_the_gpu_one_at_a_time(self):
        spans = os.path.join(self.tmp, "spans")
        procs = [self.start("ok", FAKE_HOLD="0.5", FAKE_SPANS=spans) for _ in range(3)]
        for proc in procs:
            _, err = proc.communicate(timeout=60)
            self.assertEqual(proc.returncode, 0, err)
        with open(spans) as f:
            intervals = sorted(tuple(map(float, line.split())) for line in f)
        self.assertEqual(len(intervals), 3)
        for (_, end), (start, _) in zip(intervals, intervals[1:]):
            self.assertLessEqual(end, start, "two runs held the GPU at once")

    def test_an_orphaned_chrome_keeps_the_gpu_until_it_dies(self):
        proc = self.start("hang")
        chrome, _ = self.chrome_pids()
        proc.kill()  # SIGKILL: no cleanup runs, and Chrome is left behind holding the lock
        proc.communicate()
        os.remove(self.child_pid_file)
        try:
            second = self.start("ok")
            time.sleep(1)
            self.assertIsNone(second.poll(), "a run took the GPU while an orphaned Chrome held it")
        finally:
            os.killpg(chrome, signal.SIGKILL)
        _, err = second.communicate(timeout=30)
        self.assertEqual(second.returncode, 0, err)
        self.assertIn("waiting for the GPU lock, held by: pid", err)
        self.assertIn("got the GPU lock after", err)

    def test_usage_errors(self):
        env = {**os.environ, "TMPDIR": self.tmp}
        headless = os.path.join(TOOLS, "headless.py")
        cases = [
            ([], "usage"),
            (["no/such/page"], "no page directory"),
            ([self.page, "#run", "soon"], "timeout must be a number"),
        ]
        for args, message in cases:
            r = subprocess.run([sys.executable, headless, *args], env=env, capture_output=True, text=True)
            self.assertEqual(r.returncode, 2, args)
            self.assertIn(message, r.stderr)


class LockPathTest(unittest.TestCase):
    # runtime/native/src/lock.rs follows the same rule; its test checks the same three cases.
    def test_the_lock_is_in_tmpdir_else_home(self):
        home = os.path.expanduser("~")
        self.assertEqual(headless.lock_path({"TMPDIR": "/t/x"}), "/t/x/wrela-gpu.lock")
        self.assertEqual(headless.lock_path({"TMPDIR": ""}), os.path.join(home, ".wrela-gpu.lock"))
        self.assertEqual(headless.lock_path({}), os.path.join(home, ".wrela-gpu.lock"))


class ConsoleTest(unittest.TestCase):
    def test_every_line_of_each_message_is_kept(self):
        # As Chrome 141 writes them, with another record between.
        chrome_log = (
            '[84636:49385518:1002/224334.934267:INFO:CONSOLE:2] "first line\n'
            "second line\n"
            'third line", source: file:///p.html (2)\n'
            '[84636:49385518:1002/224334.934420:INFO:CONSOLE:3] "Error: boom\n'
            '    at file:///p.html:3:15", source: file:///p.html (3)\n'
            "[84644:49385619:1002/224338.025990:ERROR:ui/display/mac/cv_display_link_mac.mm:188] "
            "CVDisplayLinkCreateWithCGDisplay failed.\n"
            '[84636:49385518:1002/224334.934461:INFO:CONSOLE:5] "trailing, source: fake (1)\n'
            'last", source: file:///p.html (5)\n'
            '[84636:49385518:1002/224334.934444:INFO:CONSOLE:4] "cut off\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            log, out = os.path.join(tmp, "chrome.log"), os.path.join(tmp, "console.log")
            with open(log, "w") as f:
                f.write(chrome_log)
            lines = headless.extract_console(log, out)
            with open(out) as f:
                written = f.read()
        expected = [
            "first line",
            "second line",
            "third line",
            "Error: boom",
            "    at file:///p.html:3:15",
            "trailing, source: fake (1)",
            "last",
            "cut off",
        ]
        self.assertEqual(lines, expected)
        self.assertEqual(written, "".join(line + "\n" for line in expected))


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


class ServeTest(PageDir, unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = serve.start(serve.QuietHandler)
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def put(self, path, body=b"x"):
        req = urllib.request.Request(f"{self.base}/{path}", data=body, method="PUT")
        try:
            with urllib.request.urlopen(req) as r:
                return r.status
        except urllib.error.HTTPError as e:
            e.close()
            return e.code

    def test_put_into_results(self):
        self.assertEqual(self.put(f"{self.page}/results/out.json", b"{}"), 201)
        with open(os.path.join(self.page_dir, "results", "out.json"), "rb") as f:
            self.assertEqual(f.read(), b"{}")
        self.assertEqual(os.listdir(os.path.join(self.page_dir, "results")), ["out.json"])

    def test_put_decodes_the_path_like_a_get(self):
        spaced = tempfile.mkdtemp(prefix=".test page-", dir=TOOLS)
        try:
            rel = urllib.parse.quote(os.path.relpath(spaced, ROOT))
            self.assertEqual(self.put(f"{rel}/results/out%20file.json", b"{}"), 201)
            self.assertEqual(os.listdir(os.path.join(spaced, "results")), ["out file.json"])
            with urllib.request.urlopen(f"{self.base}/{rel}/results/out%20file.json") as r:
                self.assertEqual(r.read(), b"{}")
        finally:
            shutil.rmtree(spaced, ignore_errors=True)

    def test_put_elsewhere_is_refused(self):
        for path in [
            f"{self.page}/out.json",
            f"{self.page}/results/deeper/out.json",
            "results/out.json",
            f"{self.page}/results/../../../../escape/results/x",
            f"{self.page}/results/%2e%2e/%2e%2e/%2e%2e/%2e%2e/escape/results/x",
        ]:
            self.assertEqual(self.put(path), 403, path)

    def test_put_through_a_symlink_out_of_the_repo_is_refused(self):
        outside = tempfile.mkdtemp()
        try:
            os.symlink(outside, os.path.join(self.page_dir, "results"))
            self.assertEqual(self.put(f"{self.page}/results/x"), 403)
            self.assertEqual(os.listdir(outside), [])
        finally:
            shutil.rmtree(outside)

    def test_only_this_machine_and_not_git(self):
        # A page elsewhere that rebinds its name to 127.0.0.1 sends its own name as the Host.
        req = urllib.request.Request(f"{self.base}/LICENSE", headers={"Host": "evil.example"})
        with self.assertRaises(urllib.error.HTTPError) as e:
            urllib.request.urlopen(req)
        self.assertEqual(e.exception.code, 403)
        e.exception.close()
        port = self.server.server_address[1]
        req = urllib.request.Request(f"{self.base}/LICENSE", headers={"Host": f"localhost:{port}"})
        with urllib.request.urlopen(req) as r:
            self.assertEqual(r.status, 200)
        for path in [".git/HEAD", "%2egit/HEAD"]:
            with self.assertRaises(urllib.error.HTTPError) as e:
                urllib.request.urlopen(f"{self.base}/{path}")
            self.assertEqual(e.exception.code, 404, path)
            e.exception.close()
        self.assertEqual(self.put(".git/results/x"), 404)

    def test_pages_are_cross_origin_isolated_with_the_right_types(self):
        with open(os.path.join(self.page_dir, "m.wasm"), "wb") as f:
            f.write(b"\0asm")
        with urllib.request.urlopen(f"{self.base}/{self.page}/m.wasm") as r:
            self.assertEqual(r.headers["Cross-Origin-Opener-Policy"], "same-origin")
            self.assertEqual(r.headers["Cross-Origin-Embedder-Policy"], "require-corp")
            self.assertEqual(r.headers["Content-Type"], "application/wasm")


if __name__ == "__main__":
    unittest.main()
