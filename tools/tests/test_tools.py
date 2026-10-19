"""Tests for tools/headless.py, tools/serve.py and tools/listening.py, with a fake Chrome (no
browser, no GPU).

    python3 -m unittest discover -s tools/tests
"""

import json
import math
import os
import random
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
import urllib.error
import urllib.parse
import urllib.request

TESTS = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.dirname(TESTS)
ROOT = os.path.dirname(TOOLS)
sys.path.insert(0, TOOLS)

import headless  # noqa: E402
import listening  # noqa: E402
import serve  # noqa: E402


def alive(pid):
    # A killed grandchild can remain as a zombie when a container's PID 1 does not reap it.
    # It has exited and holds no resources, so it did not outlive the run in the sense checked
    # here. Normal hosts reap it before this distinction matters.
    try:
        with open(f"/proc/{pid}/stat") as f:
            if f.read().split()[2] == "Z":
                return False
    except (FileNotFoundError, IndexError, OSError):
        pass
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
        self.tmp = tempfile.mkdtemp()  # the fake browser's notes
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
        results = os.path.join(self.page_dir, "results")
        os.makedirs(results)
        for stale in ("DONE", "hash.txt"):
            with open(os.path.join(results, stale), "w") as f:
                f.write("ok")
        # (3 s: under load, a shorter run can end before the fake Chrome has started.)
        status, _, err = self.run_headless("hang", timeout="3")
        self.assertEqual(status, 1)
        self.assertIn("timed out after 3s", err)
        # An earlier run's results are gone.
        self.assertFalse(os.path.exists(os.path.join(results, "hash.txt")))
        self.assert_chrome_gone()

    def test_a_stale_results_symlink_is_replaced_without_touching_its_target(self):
        outside = tempfile.mkdtemp()
        try:
            marker = os.path.join(outside, "mine")
            with open(marker, "w") as f:
                f.write("keep")
            results = os.path.join(self.page_dir, "results")
            os.symlink(outside, results)
            status, _, err = self.run_headless("ok")
            self.assertEqual(status, 0, err)
            self.assertFalse(os.path.islink(results))
            self.assertTrue(os.path.isfile(marker))
            self.assert_chrome_gone()
        finally:
            shutil.rmtree(outside, ignore_errors=True)

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

    def test_usage_errors(self):
        env = {**os.environ, "TMPDIR": self.tmp}
        headless = os.path.join(TOOLS, "headless.py")
        cases = [
            ([], "usage"),
            (["no/such/page"], "no page directory"),
            ([self.page, "#run", "soon"], "timeout must be a number"),
            ([self.page, "#run", "nan"], "timeout must be a positive finite number"),
            ([self.page, "#run", "inf"], "timeout must be a positive finite number"),
            ([self.page, "#run", "0"], "timeout must be a positive finite number"),
        ]
        for args, message in cases:
            r = subprocess.run([sys.executable, headless, *args], env=env, capture_output=True, text=True)
            self.assertEqual(r.returncode, 2, args)
            self.assertIn(message, r.stderr)

    def test_a_page_path_cant_escape_and_clear_results_outside_the_repo(self):
        outside = tempfile.mkdtemp()
        try:
            results = os.path.join(outside, "results")
            os.makedirs(results)
            marker = os.path.join(results, "mine")
            with open(marker, "w") as f:
                f.write("keep")
            env = {
                **os.environ,
                "TMPDIR": self.tmp,
                "WRELA_CHROME": os.path.join(TESTS, "fake_chrome.py"),
            }
            r = subprocess.run(
                [
                    sys.executable,
                    os.path.join(TOOLS, "headless.py"),
                    os.path.relpath(outside, ROOT),
                ],
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(r.returncode, 2)
            self.assertIn("must be inside the repository", r.stderr)
            self.assertTrue(os.path.isfile(marker))
        finally:
            shutil.rmtree(outside, ignore_errors=True)


class ConsoleTest(unittest.TestCase):
    def test_appended_chunks_keep_live_partial_messages_without_rereading(self):
        prefix = b'[84636:49385518:1002/224334.934267:INFO:CONSOLE:2] "'
        older = prefix + b'older tab", source: file:///old.html (2)\n'
        chunks = [
            (prefix[:30], ""),
            (prefix[30:] + b'first\n', "first\n"),
            (b'second \xe2', "first\nsecond \ufffd\n"),
            (b'\x82\xac\nlast", source: file:///', "first\nsecond \u20ac\nlast\", source: file:///\n"),
            (b'p.html (2)', "first\nsecond \u20ac\nlast\n"),
            (b'\r', "first\nsecond \u20ac\nlast\n"),
            (b'\n[84636:49385518:1002/224335.934267:ERROR:other] ignored\n' + prefix + b'cut off',
             "first\nsecond \u20ac\nlast\ncut off\n"),
            (b' here', "first\nsecond \u20ac\nlast\ncut off here\n"),
            (b'\n' + prefix + b'next", source: file:///p.html (2)\n',
             "first\nsecond \u20ac\nlast\ncut off here\nnext\n"),
            (prefix + b'bare\rending", source: p (2)\r',
             "first\nsecond \u20ac\nlast\ncut off here\nnext\nbare\nending\n"),
            (prefix + b'after CR", source: p (2)',
             "first\nsecond \u20ac\nlast\ncut off here\nnext\nbare\nending\nafter CR\n"),
        ]
        with tempfile.TemporaryDirectory() as tmp:
            log, out = os.path.join(tmp, "chrome.log"), os.path.join(tmp, "console.log")
            with open(log, "wb") as source:
                source.write(older)
            console = headless.ConsoleTail(log, out, len(older))
            original = open
            read_at, read_sizes, writes = [], [], []

            class Source:
                def __init__(self, source):
                    self.source = source

                def __enter__(self):
                    return self

                def __exit__(self, *args):
                    self.source.close()

                def seek(self, offset):
                    read_at.append(offset)
                    return self.source.seek(offset)

                def tell(self):
                    return self.source.tell()

                def read(self):
                    data = self.source.read()
                    read_sizes.append(len(data))
                    return data

            def tracked_open(path, mode, *args, **kwargs):
                source = original(path, mode, *args, **kwargs)
                if path == log:
                    return Source(source)
                writes.append(mode)
                return source

            offset = len(older)
            with patch.object(headless, "open", side_effect=tracked_open, create=True):
                for chunk, expected in chunks:
                    with original(log, "ab") as source:
                        source.write(chunk)
                    console.read()
                    self.assertEqual(read_at[-1], offset)
                    offset += len(chunk)
                    with original(out) as output:
                        self.assertEqual(output.read(), expected)
                    before = len(writes)
                    console.read()  # an unchanged log never rewrites the live console
                    self.assertEqual(len(writes), before)
            self.assertEqual(sum(read_sizes), sum(len(chunk) for chunk, _ in chunks))
            self.assertEqual(console.read(), chunks[-1][1].splitlines())

    def test_missing_log_then_arrival_and_final_twenty_lines(self):
        with tempfile.TemporaryDirectory() as tmp:
            log, out = os.path.join(tmp, "chrome.log"), os.path.join(tmp, "console.log")
            console = headless.ConsoleTail(log, out)
            self.assertEqual(console.read(), [])
            with open(out) as output:
                self.assertEqual(output.read(), "")
            with open(log, "w") as source:
                source.writelines(f'[1:2:1002/224334.934267:INFO:CONSOLE:2] "line {n}", source: p (2)\n'
                                  for n in range(30))
            self.assertEqual(console.read(), [f"line {n}" for n in range(10, 30)])
            with open(out) as output:
                self.assertEqual(output.read(), "".join(f"line {n}\n" for n in range(30)))

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


class ChromeDiscovery(unittest.TestCase):
    def test_platform_chrome_and_flags(self):
        found = {'google-chrome': '/usr/bin/google-chrome', 'chromium': '/usr/bin/chromium'}.get
        self.assertEqual(headless.chrome_binary({'WRELA_CHROME': '/x/chrome'}, 'linux', found), '/x/chrome')
        self.assertEqual(headless.chrome_binary({}, 'linux', found), '/usr/bin/google-chrome')
        self.assertEqual(headless.chrome_binary({}, 'linux', lambda name: None), 'google-chrome-stable')
        self.assertTrue(headless.chrome_binary({}, 'darwin', found).startswith('/Applications/'))
        self.assertIn('--use-angle=vulkan', headless.platform_args('linux'))
        self.assertIn('--password-store=basic', headless.platform_args('linux'))
        self.assertEqual(headless.platform_args('darwin'), [])


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

    def raw(self, request):
        with socket.create_connection(("127.0.0.1", self.server.server_address[1])) as sock:
            sock.sendall(request)
            sock.shutdown(socket.SHUT_WR)
            return sock.recv(4096)

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

    def test_put_rejects_an_invalid_content_length(self):
        port = self.server.server_address[1]
        for value in [b"nope", b"-1"]:
            response = self.raw(
                b"PUT /tools/results/x HTTP/1.1\r\n"
                + f"Host: localhost:{port}\r\n".encode()
                + b"Content-Length: "
                + value
                + b"\r\nConnection: close\r\n\r\n"
            )
            self.assertIn(b" 400 ", response, value)

    def test_put_rejects_a_truncated_body_without_saving_it(self):
        port = self.server.server_address[1]
        request = (
            f"PUT /{self.page}/results/truncated.bin HTTP/1.1\r\n"
            f"Host: localhost:{port}\r\n"
            "Content-Length: 4\r\nConnection: close\r\n\r\n"
        ).encode() + b"hi"
        response = self.raw(request)
        self.assertIn(b" 400 ", response)
        self.assertFalse(os.path.exists(os.path.join(self.page_dir, "results", "truncated.bin")))

    def test_put_through_a_symlink_out_of_the_repo_is_refused(self):
        outside = tempfile.mkdtemp()
        try:
            os.symlink(outside, os.path.join(self.page_dir, "results"))
            self.assertEqual(self.put(f"{self.page}/results/x"), 403)
            self.assertEqual(os.listdir(outside), [])
        finally:
            shutil.rmtree(outside)

    def test_get_and_head_refuse_symlink_aliases_outside_the_repo_and_into_git(self):
        with tempfile.TemporaryDirectory() as outside:
            with open(os.path.join(outside, "secret"), "w") as f:
                f.write("private")
            os.symlink(outside, os.path.join(self.page_dir, "outside"))
            os.mkdir(os.path.join(self.page_dir, ".git"))
            with open(os.path.join(self.page_dir, ".git", "secret"), "w") as f:
                f.write("history")
            os.symlink(".git", os.path.join(self.page_dir, "history"))
            for method in ["GET", "HEAD"]:
                for name in ["outside", "history"]:
                    req = urllib.request.Request(f"{self.base}/{self.page}/{name}/secret", method=method)
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        urllib.request.urlopen(req)
                    self.assertEqual(error.exception.code, 404)
                    error.exception.close()

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
        for path in [".git/HEAD", ".GIT/HEAD", "%2egit/HEAD"]:
            with self.assertRaises(urllib.error.HTTPError) as e:
                urllib.request.urlopen(f"{self.base}/{path}")
            self.assertEqual(e.exception.code, 404, path)
            e.exception.close()
        self.assertEqual(self.put(".git/results/x"), 404)
        self.assertEqual(self.put(".GIT/results/x"), 404)

    def test_pages_are_cross_origin_isolated_with_the_right_types(self):
        with open(os.path.join(self.page_dir, "m.wasm"), "wb") as f:
            f.write(b"\0asm")
        with urllib.request.urlopen(f"{self.base}/{self.page}/m.wasm") as r:
            self.assertEqual(r.headers["Cross-Origin-Opener-Policy"], "same-origin")
            self.assertEqual(r.headers["Cross-Origin-Embedder-Policy"], "require-corp")
            self.assertEqual(r.headers["Content-Type"], "application/wasm")

    def test_gzip_respects_accept_encoding_quality(self):
        path = os.path.join(self.page_dir, "app.js")
        with open(path, "w") as f:
            f.write("const payload = '" + "a" * 1000 + "';")
        url = f"{self.base}/{self.page}/app.js"
        cases = [
            ("gzip;q=0", None),
            ("xgzip", None),
            ("*;q=0", None),
            ("gzip;q=0, *", None),
            ("GZIP", "gzip"),
            ("br, gzip;q=0.5", "gzip"),
            ("br, *;q=0.5", "gzip"),
        ]
        for accepted, expected in cases:
            req = urllib.request.Request(url, headers={"Accept-Encoding": accepted})
            with urllib.request.urlopen(req) as response:
                self.assertEqual(response.headers.get("Content-Encoding"), expected, accepted)

    def test_gzipped_head_has_gets_representation_headers_and_no_body(self):
        path = os.path.join(self.page_dir, "app.js")
        with open(path, "w") as f:
            f.write("const payload = '" + "a" * 1000 + "';")
        url = f"{self.base}/{self.page}/app.js"
        headers = {"Accept-Encoding": "gzip"}
        with urllib.request.urlopen(urllib.request.Request(url, headers=headers)) as get:
            get_headers = (get.headers["Content-Encoding"], get.headers["Content-Length"])
        request = urllib.request.Request(url, headers=headers, method="HEAD")
        with urllib.request.urlopen(request) as head:
            self.assertEqual(
                (head.headers["Content-Encoding"], head.headers["Content-Length"]), get_headers
            )
            self.assertEqual(head.read(), b"")


class ListeningTest(unittest.TestCase):
    """listening.py's blind set: matched in loudness and length, shuffled, the key apart."""

    def test_a_blind_set_gives_nothing_away_but_the_sound(self):
        rate = 48000
        tone = lambda amp, n: [amp * math.sin(2 * math.pi * 440 * i / rate) for i in range(n)]
        # Stereo, interleaved: one loud and short, one quiet and long, one with a spike.
        loud = [x for s in tone(0.9, 4800) for x in (s, s)]
        quiet = [x for s in tone(0.05, 9600) for x in (s, s)]
        spiky = [x for s in tone(0.1, 9600) for x in (s, s)]
        spiky[100] = 1.0
        loaded = {"loud": (loud, 2, rate), "quiet": (quiet, 2, rate), "spiky": (spiky, 2, rate)}
        with tempfile.TemporaryDirectory() as out:
            key = listening.blind(loaded, out, "ABC", "key.json", rng=random.Random(7))
            self.assertEqual(sorted(v["render"] for v in key.values()), ["loud", "quiet", "spiky"])
            self.assertEqual(json.loads(open(os.path.join(out, "key.json")).read()), key)
            levels = []
            for letter in "ABC":
                samples, channels, r = listening.read_wav(os.path.join(out, f"{letter}.wav"))
                self.assertEqual((channels, r, len(samples)), (2, rate, 2 * 9600))
                # No peak past -1 dBFS (and a 16-bit step for rounding).
                self.assertLessEqual(max(abs(x) for x in samples), 10 ** (-1 / 20) + 1 / 32768)
                levels.append(listening.rms(samples))
            # One level for all, set by the spike's peak.
            self.assertLess(max(levels) / min(levels), 1.001)

    def test_the_gate_counts_wrelas_pairs_and_passes_at_12_of_16(self):
        key = {f"{n:02}": {"wrela": "A" if n % 3 else "B"} for n in range(1, 17)}
        right = "".join(f"{n} {v['wrela']}\n" for n, v in key.items())
        self.assertEqual(listening.score(key, right), {"wrela": 16, "of": 16, "p": 0.0, "passes": True})
        # Four wrong: 12 of 16, p = 0.0384.
        flip = {"A": "B", "B": "A"}
        some = "".join(f"{n} {flip[v['wrela']] if int(n) <= 4 else v['wrela']}\n" for n, v in key.items())
        self.assertEqual(listening.score(key, some), {"wrela": 12, "of": 16, "p": 0.0384, "passes": True})
        five = "".join(f"{n} {flip[v['wrela']] if int(n) <= 5 else v['wrela']}\n" for n, v in key.items())
        self.assertFalse(listening.score(key, five)["passes"])
        with self.assertRaises(SystemExit):
            listening.score(key, "01 A\n")

    def test_an_excerpt_is_its_bars_faded_in_and_out(self):
        described = {"bars": 2, "beats": [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0]}
        self.assertEqual(listening.excerpt_times(described, 2, 2), (1.5, 3.0))
        rate = 1000
        samples = [1.0] * (2 * 4 * rate)
        out = listening.cut(samples, 2, rate, 1.0, 2.0)
        self.assertEqual(len(out), 2 * rate)
        self.assertEqual((out[0], out[1]), (0.0, 0.0))
        self.assertEqual(out[2 * 500], 1.0)
        self.assertEqual(out[-1], 0.0)


if __name__ == "__main__":
    unittest.main()
