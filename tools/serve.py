"""Static server for the repository, plus PUT into any `results/` directory so a page can save
its own JSON and screenshots. Local use only: binds 127.0.0.1, answers only requests addressed
to this machine by name (a page elsewhere can't reach it through DNS rebinding), and serves
nothing under `.git`. Pages are served at their path relative to the repo root,
cross-origin isolated (COOP/COEP), as the browser runtime needs."""

import gzip
import http.server
import io
import os
import sys
import threading
import time
import urllib.parse

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # the repo root


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".js": "text/javascript",
        ".mjs": "text/javascript",
        ".wasm": "application/wasm",
        ".wgsl": "text/plain",
        ".json": "application/json",
    }

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=ROOT, **kwargs)

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        super().end_headers()

    LOCAL_HOSTS = ("127.0.0.1", "localhost", "[::1]")

    def refused(self):
        """Refuses (and answers) a request for another host, or for anything under `.git`."""
        host = (self.headers.get("Host") or "").lower()
        port = self.server.server_address[1]
        if host not in self.LOCAL_HOSTS and host not in [f"{h}:{port}" for h in self.LOCAL_HOSTS]:
            self.send_error(403, "this server answers only requests for this machine")
            return True
        path = urllib.parse.unquote(self.path.split("?", 1)[0].split("#", 1)[0])
        if ".git" in path.split("/"):
            self.send_error(404, "the repository's history isn't served")
            return True
        return False

    def do_GET(self):
        if self.refused():
            return
        if not self.send_gzipped():
            super().do_GET()

    # What a host compresses when the browser accepts it, as a game's host would.
    GZIPPED = (".wasm", ".js", ".mjs", ".wgsl", ".json", ".bin", ".html")

    def send_gzipped(self):
        """Sends the file gzipped (as a static host would) if the request accepts gzip and it's a
        kind a host compresses: whether it did. The test pages' load times and bytes then are
        what a player's would be."""
        if "gzip" not in (self.headers.get("Accept-Encoding") or ""):
            return False
        path = self.translate_path(self.path)
        if not path.endswith(self.GZIPPED) or not os.path.isfile(path):
            return False
        body = gzipped(path)
        self.send_response(200)
        self.send_header("Content-Type", self.guess_type(path))
        self.send_header("Content-Encoding", "gzip")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.copyfile(io.BytesIO(body), self.wfile)
        return True

    def copyfile(self, source, outputfile):
        """Sends a file, at most `WRELA_THROTTLE` bits a second over every request at once when
        that's set (a slow network: #55 AC7's 5 Mbit/s)."""
        if THROTTLE.rate is None:
            return super().copyfile(source, outputfile)
        while True:
            chunk = source.read(16384)
            if not chunk:
                return
            THROTTLE.take(len(chunk))
            outputfile.write(chunk)

    def do_HEAD(self):
        if not self.refused():
            super().do_HEAD()

    def results_path(self):
        """The file a PUT may write, or None. Only a file directly inside a `results/` directory
        under the repo root; never through a symlink that leads out of the repo. The path is
        URL-decoded first, as a GET's is, so both name the same file."""
        path = urllib.parse.unquote(self.path.split("?", 1)[0].split("#", 1)[0])
        rel = os.path.normpath(path.lstrip("/"))
        parts = rel.split(os.sep)
        if rel.startswith("..") or os.path.isabs(rel) or len(parts) < 3 or parts[-2] != "results":
            return None
        dest = os.path.join(ROOT, rel)
        parent = os.path.realpath(os.path.dirname(dest))
        root = os.path.realpath(ROOT)
        if os.path.commonpath([parent, root]) != root:
            return None
        return dest

    def do_PUT(self):
        if self.refused():
            return
        dest = self.results_path()
        if dest is None:
            self.send_error(403, "PUT is only allowed into a results/ directory")
            return
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        tmp = f"{dest}.{os.getpid()}.{id(self)}.tmp"
        with open(tmp, "wb") as f:
            f.write(body)
        os.replace(tmp, dest)  # readers never see a half-written file
        self.send_response(201)
        self.end_headers()
        self.on_put(os.path.relpath(dest, ROOT), body)

    def on_put(self, rel, body):
        """Hook for tools/headless.py."""


_GZIPPED = {}  # (path, size, mtime) -> the file gzipped
_GZIPPED_LOCK = threading.Lock()


def gzipped(path):
    """The file at `path`, gzipped: once for each version of it."""
    st = os.stat(path)
    key = (path, st.st_size, st.st_mtime_ns)
    with _GZIPPED_LOCK:
        body = _GZIPPED.get(key)
    if body is None:
        with open(path, "rb") as f:
            body = gzip.compress(f.read(), compresslevel=6)
        with _GZIPPED_LOCK:
            _GZIPPED[key] = body
    return body


class Throttle:
    """A rate every request shares (bytes a second), or none: `WRELA_THROTTLE` in bits a second.
    A request takes the bytes it's about to send, waiting until the rate allows them."""

    def __init__(self):
        bits = os.environ.get("WRELA_THROTTLE")
        self.rate = float(bits) / 8.0 if bits else None
        self.lock = threading.Lock()
        self.next = time.monotonic()

    def take(self, n):
        with self.lock:
            now = time.monotonic()
            start = max(now, self.next)
            self.next = start + n / self.rate
        time.sleep(max(start + n / self.rate - time.monotonic(), 0.0))


THROTTLE = Throttle()


class QuietHandler(Handler):
    """`Handler` without the request log, for tools and tests."""

    def log_message(self, *args):
        pass


def start(handler=Handler, port=0):
    """Serves the repo with `handler` on 127.0.0.1:`port` (0: a free port) from a daemon thread.
    Returns the server: `server.server_address[1]` is the port, `server.shutdown()` stops it."""
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


if __name__ == "__main__":
    start(port=int(sys.argv[1]) if len(sys.argv) > 1 else 8417)
    threading.Event().wait()  # until interrupted
