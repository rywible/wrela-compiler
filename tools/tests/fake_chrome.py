#!/usr/bin/env python3
"""Stands in for Chrome in tools/tests: loads nothing, and acts out FAKE_MODE against the URL."""

import os
import subprocess
import sys
import time
import urllib.request

url = sys.argv[-1]
base = url.rsplit("/", 1)[0]  # http://127.0.0.1:<port>/<page>
mode = os.environ["FAKE_MODE"]
spans = os.environ.get("FAKE_SPANS")


def put(name, body):
    req = urllib.request.Request(f"{base}/results/{name}", data=body.encode(), method="PUT")
    urllib.request.urlopen(req).read()


# A child in Chrome's process group, as a GPU process would be; tests check it dies with Chrome.
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
with open(os.environ["FAKE_CHILD_PID"], "w") as f:
    f.write(f"{os.getpid()} {child.pid}")

print('[1:2:INFO:CONSOLE:7] "hello from the page", source: http://x/main.js (7)', file=sys.stderr, flush=True)
print(
    '[1:2:1002/224334.934420:INFO:CONSOLE:9] "pipeline `p` failed to build:\n'
    '3:5: unresolved identifier", source: http://x/worker.js (9)',
    file=sys.stderr,
    flush=True,
)
start = time.time()
time.sleep(float(os.environ.get("FAKE_HOLD", "0")))
if spans:  # the time this run held the GPU, written before DONE ends it
    with open(spans, "a") as f:
        f.write(f"{start} {time.time()}\n")
if mode == "ok":
    put("DONE", "ok")
elif mode == "fail":
    put("DONE", "WGSL compile error")
elif mode == "exit":
    sys.exit(4)
time.sleep(60)  # like Chrome, never exits by itself
