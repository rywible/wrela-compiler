"""Static server for the repository, plus PUT into any `results/` directory so a page can save
its own JSON and screenshots. Local use only: binds 127.0.0.1. Pages are served at their path
relative to the repo root."""

import http.server
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # the repo root


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=ROOT, **kwargs)

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_PUT(self):
        rel = os.path.normpath(self.path.split("?", 1)[0].lstrip("/"))
        parts = rel.split(os.sep)
        if rel.startswith("..") or len(parts) < 3 or parts[-2] != "results":
            self.send_error(403, "PUT is only allowed into a results/ directory")
            return
        dest = os.path.join(ROOT, rel)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        length = int(self.headers.get("Content-Length", 0))
        with open(dest, "wb") as f:
            f.write(self.rfile.read(length))
        self.send_response(201)
        self.end_headers()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8417
    http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
