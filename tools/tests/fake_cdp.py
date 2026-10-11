#!/usr/bin/env python3
"""A minimal Chrome debugging endpoint for session lifecycle tests; no browser or GPU."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import threading
from urllib.parse import unquote
from urllib.request import Request, urlopen

profile = Path(next(a.split('=', 1)[1] for a in sys.argv if a.startswith('--user-data-dir=')))
targets = set()
serial = 0


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def respond(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_PUT(self):
        global serial
        serial += 1
        target = str(serial)
        targets.add(target)
        url = unquote(self.path.split('?', 1)[1])
        print(f'[1:2:INFO:CONSOLE:7] "tab {target}", source: {url} (7)', file=sys.stderr, flush=True)
        self.respond({'id': target})
        if 'timeout' not in url:
            def done():
                req = Request(url.rsplit('/', 1)[0] + '/results/DONE', data=b'ok', method='PUT')
                with urlopen(req) as response:
                    response.read()
            threading.Thread(target=done, daemon=True).start()

    def do_GET(self):
        if self.path.startswith('/json/close/'):
            targets.remove(self.path.rsplit('/', 1)[1])
            self.respond({})
        else:
            self.respond([{'id': target} for target in targets])


server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
(profile / 'DevToolsActivePort').write_text(str(server.server_port) + '\n')
server.serve_forever()
