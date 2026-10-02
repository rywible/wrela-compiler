"""Static server for the repository, plus PUT into any `results/` directory so a page can save
its own JSON and screenshots. Local use only: binds 127.0.0.1. Pages are served at their path
relative to the repo root, cross-origin isolated (COOP/COEP), as the browser runtime needs."""

import http.server
import os
import sys

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

    def results_path(self):
        """The file a PUT may write, or None. Only a file directly inside a `results/` directory
        under the repo root; never through a symlink that leads out of the repo."""
        rel = os.path.normpath(self.path.split("?", 1)[0].lstrip("/"))
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


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8417
    http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
