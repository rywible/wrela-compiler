#!/usr/bin/env python3
"""Runs a page in headless Chrome stable (real GPU, throwaway profile) and waits for it to finish.

    tools/headless.py <page-dir relative to the repo root> [hash] [timeout-seconds]
    tools/headless.py runtime/browser/tests/hello-field '#run' 120

The page finishes by PUTting `results/DONE`: the body `ok` means it passed, anything else is the
failure message. Its other PUTs land in `<page-dir>/results/`, and its console output (exceptions
and WGSL compile errors included) goes to `<page-dir>/results/console.log`.

Each run serves the repo on its own free port (tools/serve.py's handler, cross-origin isolated),
so concurrent runs don't need a server and can't collide on one.

Exit status: 0 the page wrote `ok`; 1 it failed, timed out, or Chrome exited; 2 a setup error;
3 the GPU lock wasn't free within an hour.

Environment: WRELA_CHROME overrides the Chrome binary.
"""

import fcntl
import http.server
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time

import serve

CHROME = os.environ.get(
    "WRELA_CHROME", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
)
LOCK_WAIT_LIMIT = 3600  # seconds

# One GPU user at a time. On 2026-10-01, ~12 concurrent headless runs starved WindowServer of GPU
# time for over 5 s; macOS's watchdog killed it and the whole desktop reset. Runs queue on an
# flock(2), which the kernel releases when its holder dies, so a crashed run never leaves a stale
# lock. Chrome inherits the descriptor: if this script is SIGKILLed, the lock stays held until
# Chrome is gone too. The lock lives in the per-user temp directory ($TMPDIR on macOS, which is
# also Rust's std::env::temp_dir()), not in world-writable /tmp.
LOCK_PATH = os.path.join(tempfile.gettempdir(), "wrela-gpu.lock")


def fail(status, message):
    print(message, file=sys.stderr)
    sys.exit(status)


def acquire_gpu_lock(page):
    try:
        fd = os.open(LOCK_PATH, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    except OSError as e:
        fail(2, f"can't open the GPU lock {LOCK_PATH}: {e}")
    waited = 0.0
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            break
        except BlockingIOError:
            if waited == 0:
                holder = os.pread(fd, 512, 0).decode(errors="replace").strip() or "unknown"
                print(f"waiting for the GPU lock, held by: {holder}", file=sys.stderr)
            if waited >= LOCK_WAIT_LIMIT:
                fail(3, f"gave up waiting for the GPU lock ({LOCK_PATH}) after an hour")
            time.sleep(0.5)
            waited += 0.5
    if waited:
        print(f"got the GPU lock after {waited:.0f}s", file=sys.stderr)
    os.ftruncate(fd, 0)
    os.pwrite(fd, f"pid {os.getpid()}, {page}\n".encode(), 0)
    return fd


def start_server(page, done):
    """Serves the repo on a free port; sets `done` to the DONE body when the page writes it."""
    done_path = os.path.join(page, "results", "DONE")

    class Handler(serve.Handler):
        def on_put(self, rel, body):
            if rel == done_path:
                done.append(body.decode(errors="replace").strip())

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


CONSOLE_LINE = re.compile(r"CONSOLE[^\]]*\] (.*?)(?:, source: .*)?$")


def extract_console(chrome_log, console_log):
    lines = []
    try:
        with open(chrome_log, errors="replace") as f:
            for line in f:
                m = CONSOLE_LINE.search(line.rstrip("\n"))
                if m:
                    lines.append(m.group(1))
    except FileNotFoundError:
        pass
    with open(console_log, "w") as f:
        f.writelines(line + "\n" for line in lines)
    return lines


def stop(proc):
    """Stops Chrome and every process it started (they share its process group), including any
    left behind when Chrome itself exits first."""
    if proc.poll() is None:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
            proc.wait(2.0)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            pass
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    proc.wait()


def interrupt(signum, frame):
    """SIGTERM and SIGHUP take the same cleanup path as Ctrl-C."""
    raise KeyboardInterrupt


def main(argv):
    if not 2 <= len(argv) <= 4:
        fail(2, "usage: tools/headless.py <page-dir> [hash] [timeout-seconds]")
    page = os.path.normpath(argv[1]).strip("/")
    fragment = argv[2] if len(argv) > 2 else "#run"
    try:
        timeout = float(argv[3]) if len(argv) > 3 else 120.0
    except ValueError:
        fail(2, f"timeout must be a number of seconds, not {argv[3]!r}")
    page_dir = os.path.join(serve.ROOT, page)
    if not os.path.isdir(page_dir):
        fail(2, f"no page directory at {page_dir}")
    if not os.access(CHROME, os.X_OK):
        fail(2, f"Chrome isn't at {CHROME} (set WRELA_CHROME)")

    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, interrupt)

    lock = acquire_gpu_lock(page)
    results = os.path.join(page_dir, "results")
    os.makedirs(results, exist_ok=True)
    for stale in ("DONE", "console.log"):
        try:
            os.remove(os.path.join(results, stale))
        except FileNotFoundError:
            pass

    done = []
    server = start_server(page, done)
    url = f"http://127.0.0.1:{server.server_address[1]}/{page}/{fragment}"
    profile = tempfile.mkdtemp(prefix="wrela-chrome.")
    chrome_log = os.path.join(profile, "chrome.log")
    start = time.monotonic()
    status, reason = 1, None
    proc = None
    try:
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
                    "--enable-logging=stderr",
                    "--v=0",
                    "--window-size=1920,1080",
                    url,
                ],
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=log,
                pass_fds=(lock,),
                start_new_session=True,
            )
        while True:
            if done:
                status, reason = (0, None) if done[0] == "ok" else (1, f"page failed: {done[0]}")
                break
            if proc.poll() is not None:
                reason = f"chrome exited early (status {proc.returncode})"
                break
            if time.monotonic() - start >= timeout:
                reason = f"timed out after {timeout:.0f}s"
                break
            time.sleep(0.1)
    except KeyboardInterrupt:
        status, reason = 1, "interrupted"
    finally:
        if proc is not None:
            stop(proc)
        server.shutdown()
        lines = extract_console(chrome_log, os.path.join(results, "console.log"))
        shutil.rmtree(profile, ignore_errors=True)
        os.close(lock)

    print(f"elapsed {time.monotonic() - start:.1f}s; console: {os.path.relpath(results)}/console.log")
    for line in lines[-20:]:
        print(f"  {line}")
    if reason:
        print(reason, file=sys.stderr)
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv))
