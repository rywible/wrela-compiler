"""Tests for tools/headless.py and tools/serve.py, with a fake Chrome (no browser, no GPU).

    python3 -m unittest discover -s tools/tests
"""

import http.server
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request

TESTS = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.dirname(TESTS)
ROOT = os.path.dirname(TOOLS)
sys.path.insert(0, TOOLS)

import serve  # noqa: E402


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


class HeadlessTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()  # the lock and the fake's notes; TMPDIR isolates the lock
        self.page_dir = tempfile.mkdtemp(prefix=".test-page-", dir=TOOLS)
        self.page = os.path.relpath(self.page_dir, ROOT)
        self.child_pid_file = os.path.join(self.tmp, "child.pid")

    def tearDown(self):
        shutil.rmtree(self.page_dir, ignore_errors=True)
        shutil.rmtree(self.tmp, ignore_errors=True)

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
            self.assertEqual(f.read(), '"hello from the page"\n')
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


class ServeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        class Quiet(serve.Handler):
            def log_message(self, *args):
                pass

        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Quiet)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.page_dir = tempfile.mkdtemp(prefix=".test-page-", dir=TOOLS)
        self.page = os.path.relpath(self.page_dir, ROOT)

    def tearDown(self):
        shutil.rmtree(self.page_dir, ignore_errors=True)

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

    def test_put_elsewhere_is_refused(self):
        for path in [
            f"{self.page}/out.json",
            f"{self.page}/results/deeper/out.json",
            "results/out.json",
            f"{self.page}/results/../../../../escape/results/x",
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

    def test_pages_are_cross_origin_isolated_with_the_right_types(self):
        with open(os.path.join(self.page_dir, "m.wasm"), "wb") as f:
            f.write(b"\0asm")
        with urllib.request.urlopen(f"{self.base}/{self.page}/m.wasm") as r:
            self.assertEqual(r.headers["Cross-Origin-Opener-Policy"], "same-origin")
            self.assertEqual(r.headers["Cross-Origin-Embedder-Policy"], "require-corp")
            self.assertEqual(r.headers["Content-Type"], "application/wasm")


if __name__ == "__main__":
    unittest.main()
