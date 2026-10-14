#!/usr/bin/env python3
"""Runs a page in headless Chrome stable (real GPU, throwaway profile) and waits for it to finish.

    tools/headless.py <page-dir relative to the repo root> [hash] [timeout-seconds]
    tools/headless.py runtime/browser/tests/hello-field '#run' 120
    tools/headless.py --url <url> <page-dir> [timeout-seconds]

With --url, another server already serves the page (the studio server, which answers the lens's
posts): Chrome opens the URL, and the run is done when the page's `results/DONE` file appears
(the page-dir may then be any directory).

The page finishes by PUTting `results/DONE`: the body `ok` means it passed, anything else is the
failure message. Its other PUTs land in `<page-dir>/results/`, which each run empties first, and
its console output (exceptions and WGSL compile errors included) goes to
`<page-dir>/results/console.log`.

Each run serves the repo on its own free port (tools/serve.py's handler, cross-origin isolated),
so concurrent runs don't need a server and can't collide on one.

Exit status: 0 the page wrote `ok`; 1 it failed, timed out, or Chrome exited; 2 a setup error.

Environment: WRELA_CHROME overrides the Chrome binary (by default the macOS app, or on Linux the
first Chrome on PATH, with the flags that expose its hardware WebGPU adapter); WRELA_CHROME_ARGS
adds arguments to its command line (split as a shell would). Rendering requires a hardware
WebGPU adapter.
WRELA_THROTTLE (bits a second) slows the server's sending, every request sharing the rate
(tools/serve.py).
"""

from collections import deque
import math
import os
import re
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

import serve

LINUX_CHROMES = ("google-chrome-stable", "google-chrome", "chromium", "chromium-browser")


def chrome_binary(environ=os.environ, platform=sys.platform, which=shutil.which):
    """WRELA_CHROME, else the platform's Chrome: the macOS app, or the first on PATH."""
    if environ.get("WRELA_CHROME"):
        return environ["WRELA_CHROME"]
    if platform == "darwin":
        return "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    return next(filter(None, map(which, LINUX_CHROMES)), LINUX_CHROMES[0])


def platform_args(platform=sys.platform):
    """Linux Chrome keeps WebGPU behind flags, and without Vulkan through ANGLE it offers only
    its software adapter, which the runtime refuses."""
    if platform.startswith("linux"):
        return ["--enable-unsafe-webgpu", "--enable-features=Vulkan", "--use-angle=vulkan",
                "--ignore-gpu-blocklist"]
    return []


CHROME = chrome_binary()
def fail(status, message):
    print(message, file=sys.stderr)
    sys.exit(status)


def start_server(page, done):
    """Serves the repo on a free port; sets `done` to the DONE body when the page writes it."""
    done_path = os.path.join(page, "results", "DONE")

    class Handler(serve.QuietHandler):
        def on_put(self, rel, body):
            if rel == done_path:
                done.append(body.decode(errors="replace").strip())

    return serve.start(Handler)


# Chrome logs a console message as one record, `[...:INFO:CONSOLE:7] "<text>", source: <url> (7)`,
# and the text keeps its line breaks: a WGSL error or a stack trace spans several lines.
CONSOLE_START = re.compile(r'CONSOLE[^\]]*\] "(.*)$')
CONSOLE_END = re.compile(r'^(.*)", source: .* \(\d+\)$')
LOG_RECORD = re.compile(r"^\[\d+:\d+:\d{4}/\d{6}\.\d+:")


class ConsoleTail:
    """Tail Chrome's appended records while keeping incomplete console lines live."""

    def __init__(self, chrome_log, console_log, offset=0):
        self.chrome_log, self.console_log = chrome_log, console_log
        self.offset, self.written = offset, 0
        self.pending = b""
        self.message = False
        self.lines = deque(maxlen=20)
        self.preview = None
        with open(console_log, "wb"):
            pass

    @staticmethod
    def parse(line, message):
        if message and LOG_RECORD.match(line):
            message = False  # the preceding message ended without a source
        if not message:
            start = CONSOLE_START.search(line)
            if not start:
                return None, False
            line = start.group(1)
        end = CONSOLE_END.match(line)
        return end.group(1) if end else line, end is None

    def read(self):
        try:
            with open(self.chrome_log, "rb") as source:
                source.seek(self.offset)
                added = source.read()
                self.offset = source.tell()
        except FileNotFoundError:
            added = b""
        if added:
            data = self.pending + added
            complete = []
            at = 0
            for end in re.finditer(br"\r\n|\r|\n", data):
                # A CR at EOF may become CRLF on the next write. Its live preview still
                # appears now; wait to consume it so the LF cannot create a blank line.
                if end.group() == b"\r" and end.end() == len(data):
                    break
                line, self.message = self.parse(data[at:end.start()].decode(errors="replace"), self.message)
                if line is not None:
                    complete.append(line)
                at = end.end()
            self.pending = data[at:]
            self.preview = self.parse(self.pending.rstrip(b"\r").decode(errors="replace"), self.message)[0] \
                if self.pending else None
            body = "".join(line + "\n" for line in complete).encode()
            preview = (self.preview + "\n").encode() if self.preview is not None else b""
            # Only the incomplete last line can change. Complete lines stay on disk.
            with open(self.console_log, "r+b") as output:
                output.seek(self.written)
                output.truncate()
                output.write(body + preview)
            self.written += len(body)
            self.lines.extend(complete)
        lines = [*self.lines, *([self.preview] if self.preview is not None else [])]
        return lines[-20:]


def extract_console(chrome_log, console_log, offset=0):
    """Write a snapshot of every console message; return all its lines."""
    ConsoleTail(chrome_log, console_log, offset).read()
    with open(console_log) as output:
        return [line.rstrip("\n") for line in output]


def adopt_chrome_children():
    """Reap Chrome's orphaned children ourselves on Linux, even with a non-reaping PID 1."""
    if sys.platform == "linux":
        import ctypes

        libc = ctypes.CDLL(None, use_errno=True)
        # PR_SET_CHILD_SUBREAPER: descendants become our children when their parent exits.
        if libc.prctl(36, 1, 0, 0, 0) != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))


def stop(proc):
    """Stops Chrome and every process it started (they share its process group), including any
    left behind when Chrome itself exits first."""
    # macOS refuses (EPERM) to signal a group whose processes have all exited but not yet been
    # reaped: that group is gone too.
    if proc.poll() is None:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
            proc.wait(2.0)
        except (ProcessLookupError, PermissionError, subprocess.TimeoutExpired):
            pass
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    proc.wait()
    if sys.platform == "linux":
        # Only this Chrome's process group: leave any unrelated subprocesses to their owners.
        while True:
            try:
                os.waitpid(-proc.pid, 0)
            except ChildProcessError:
                break


def interrupt(signum, frame):
    """SIGTERM and SIGHUP take the same cleanup path as Ctrl-C."""
    raise KeyboardInterrupt


def main(argv):
    url = None
    if len(argv) > 1 and argv[1] == "--url":
        if not 4 <= len(argv) <= 5:
            fail(2, "usage: tools/headless.py --url <url> <page-dir> [timeout-seconds]")
        url, page_dir = argv[2], os.path.abspath(argv[3])
        page = page_dir
        timeout_arg = argv[4] if len(argv) > 4 else None
    else:
        if not 2 <= len(argv) <= 4:
            fail(2, "usage: tools/headless.py <page-dir> [hash] [timeout-seconds]")
        page = os.path.normpath(argv[1]).strip("/")
        fragment = argv[2] if len(argv) > 2 else "#run"
        timeout_arg = argv[3] if len(argv) > 3 else None
        page_dir = os.path.join(serve.ROOT, page)
        root = os.path.realpath(serve.ROOT)
        if os.path.commonpath([os.path.realpath(page_dir), root]) != root:
            fail(2, f"page directory must be inside the repository, not {argv[1]}")
    try:
        timeout = float(timeout_arg) if timeout_arg else 120.0
    except ValueError:
        fail(2, f"timeout must be a number of seconds, not {timeout_arg!r}")
    if not math.isfinite(timeout) or timeout <= 0:
        fail(2, f"timeout must be a positive finite number of seconds, not {timeout_arg!r}")
    if not os.path.isdir(page_dir):
        fail(2, f"no page directory at {page_dir}")
    if not os.access(CHROME, os.X_OK):
        fail(2, f"Chrome isn't at {CHROME} (set WRELA_CHROME)")

    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, interrupt)

    # What an earlier run left (a stale DONE above all) isn't this run's.
    results = os.path.join(page_dir, "results")
    try:
        mode = os.lstat(results).st_mode
        if stat.S_ISDIR(mode) and not stat.S_ISLNK(mode):
            shutil.rmtree(results)
        else:
            os.unlink(results)
    except FileNotFoundError:
        pass
    except OSError as e:
        fail(2, f"can't clear {results}: {e}")
    try:
        os.makedirs(results)
    except OSError as e:
        fail(2, f"can't make {results}: {e}")

    done = []
    server = None
    if url is None:
        server = start_server(page, done)
        url = f"http://127.0.0.1:{server.server_address[1]}/{page}/{fragment}"
    done_file = os.path.join(results, "DONE")
    profile = tempfile.mkdtemp(prefix="wrela-chrome.")
    chrome_log = os.path.join(profile, "chrome.log")
    start = time.monotonic()
    status, reason = 1, None
    proc = None
    tab = None
    shared = None
    offset = 0
    console = None
    try:
        shared = os.environ.get("WRELA_TEST_CHROME")
        if shared:
            from chrome_session import request
            tab = request(shared, "start", {"url": url})
            chrome_log, offset = tab["log"], tab["offset"]
        else:
            adopt_chrome_children()
            with open(chrome_log, "wb") as log:
                proc = subprocess.Popen(
                    [
                        CHROME,
                        "--headless=new",
                        f"--user-data-dir={profile}",
                        "--no-first-run",
                        "--no-default-browser-check",
                        "--disable-extensions",
                        "--disable-background-timer-throttling",
                        "--disable-renderer-backgrounding",
                        # A test page plays its voice without a click (test mode's `live`).
                        "--autoplay-policy=no-user-gesture-required",
                        # Timestamp queries at full resolution, not quantized to 100 µs.
                        "--enable-webgpu-developer-features",
                        "--enable-logging=stderr",
                        "--v=0",
                        "--window-size=1920,1080",
                        *platform_args(),
                        *shlex.split(os.environ.get("WRELA_CHROME_ARGS", "")),
                        url,
                    ],
                    stdin=subprocess.DEVNULL,
                    stdout=log,
                    stderr=log,
                    start_new_session=True,
                )
        console = ConsoleTail(chrome_log, os.path.join(results, "console.log"), offset)
        while True:
            if server is None and not done and os.path.exists(done_file):
                with open(done_file, errors="replace") as f:
                    done.append(f.read().strip())
            if done:
                status, reason = (0, None) if done[0] == "ok" else (1, f"page failed: {done[0]}")
                break
            if (shared and not request(shared, "alive", {})["alive"]) or (proc is not None and proc.poll() is not None):
                reason = "chrome exited early" if shared else f"chrome exited early (status {proc.returncode})"
                break
            if time.monotonic() - start >= timeout:
                reason = f"timed out after {timeout:.0f}s"
                break
            console.read()
            time.sleep(0.1)
    except KeyboardInterrupt:
        status, reason = 1, "interrupted"
    finally:
        if tab is not None:
            try:
                request(shared, "close", {"target": tab["target"]})
            except Exception as error:
                status, reason = 1, f"could not close the browser test tab: {error}"
        if proc is not None:
            stop(proc)
        if server is not None:
            server.shutdown()
            server.server_close()
        if console is None:
            console = ConsoleTail(chrome_log, os.path.join(results, "console.log"), offset)
        lines = console.read()
        shutil.rmtree(profile, ignore_errors=True)

    print(f"elapsed {time.monotonic() - start:.1f}s; console: {os.path.relpath(results)}/console.log")
    for line in lines[-20:]:
        print(f"  {line}")
    if reason:
        print(reason, file=sys.stderr)
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv))
