"""One lazy, disposable Chrome for a check, with a fresh tab for each test scenario.

headless.py retains its per-scenario server, results and timeout. Only the
browser process is reused. Test threads run scenarios at the same time, each in its own tab;
headless.py keeps each scenario's console to the records its own page logged.
"""
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import shlex
import socket
import struct
import subprocess
import threading
import time
from urllib.parse import quote, urlparse
from urllib.request import Request, urlopen


def request(endpoint, action, payload=None, timeout=20):
    req = Request(endpoint + '/' + action, data=json.dumps(payload).encode() if payload is not None else None,
                  headers={'Content-Type': 'application/json'})
    with urlopen(req, timeout=timeout) as response:
        return json.load(response)


class Socket:
    """A WebSocket client (RFC 6455) on loopback: just what Chrome's DevTools protocol needs, text
    frames out (masked, as a client's must be) and whole text frames in."""

    GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'

    def __init__(self, url, timeout=20):
        u = urlparse(url)
        if u.hostname != '127.0.0.1':
            raise ValueError('DevTools is only on loopback')
        self.sock = socket.create_connection((u.hostname, u.port), timeout=timeout)
        key = base64.b64encode(secrets.token_bytes(16)).decode()
        self.sock.sendall((f'GET {u.path} HTTP/1.1\r\nHost: {u.hostname}:{u.port}\r\n'
                           'Upgrade: websocket\r\nConnection: Upgrade\r\n'
                           f'Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
        head = b''
        while b'\r\n\r\n' not in head:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ValueError('DevTools closed the connection')
            head += chunk
        header, self.buffer = head.split(b'\r\n\r\n', 1)
        accept = base64.b64encode(hashlib.sha1((key + self.GUID).encode()).digest())
        if b' 101 ' not in header.split(b'\r\n')[0] or accept not in header:
            raise ValueError('DevTools refused the WebSocket')

    def _read(self, n):
        while len(self.buffer) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ValueError('DevTools closed the connection')
            self.buffer += chunk
        out, self.buffer = self.buffer[:n], self.buffer[n:]
        return out

    def send(self, text, opcode=1):
        data = text.encode() if isinstance(text, str) else text
        n = len(data)
        head = bytes([0x80 | opcode])
        if n < 126:
            head += bytes([0x80 | n])
        elif n < 65536:
            head += bytes([0x80 | 126]) + struct.pack('>H', n)
        else:
            head += bytes([0x80 | 127]) + struct.pack('>Q', n)
        mask = secrets.token_bytes(4)
        self.sock.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def receive(self):
        """The next whole text message; pings are answered, a close ends it."""
        message = b''
        while True:
            first, second = self._read(2)
            n = second & 0x7f
            if n == 126:
                n = struct.unpack('>H', self._read(2))[0]
            elif n == 127:
                n = struct.unpack('>Q', self._read(8))[0]
            mask = self._read(4) if second & 0x80 else None
            data = self._read(n)
            if mask:
                data = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
            opcode = first & 0x0f
            if opcode == 8:
                raise ValueError('DevTools closed the WebSocket')
            if opcode == 9:
                self.send(data, opcode=10)
                continue
            if opcode in (0, 1):
                message += data
                if first & 0x80:
                    return message.decode()

    def close(self):
        self.sock.close()


def cdp(port, target, commands):
    """Runs DevTools `commands` on tab `target`, in order: each `{"method", "params"}` waits for
    its answer; `{"wait": s}` waits s seconds; `{"until": js, "timeout": s}` evaluates `js` in the
    page until it's true (or fails after `timeout`, 20 s by default). Returns the answers."""
    ws = Socket(f'ws://127.0.0.1:{port}/devtools/page/{target}')
    answers = []
    try:
        n = 0
        def call(method, params):
            nonlocal n
            n += 1
            ws.send(json.dumps({'id': n, 'method': method, 'params': params}))
            while True:
                message = json.loads(ws.receive())
                if message.get('id') == n:
                    if 'error' in message:
                        raise ValueError(f'{method}: {message["error"].get("message")}')
                    return message.get('result', {})
        for command in commands:
            if 'wait' in command:
                time.sleep(float(command['wait']))
            elif 'until' in command:
                deadline = time.monotonic() + float(command.get('timeout', 20))
                while True:
                    result = call('Runtime.evaluate', {'expression': command['until'], 'returnByValue': True})
                    if result.get('result', {}).get('value') is True:
                        break
                    if time.monotonic() >= deadline:
                        raise ValueError(f'the page never had {command["until"]}')
                    time.sleep(0.05)
            else:
                answers.append(call(command['method'], command.get('params', {})))
    finally:
        ws.close()
    return answers


class ChromeSession:
    # How long Chrome may take to publish its debugging port.
    STARTUP_SECONDS = 15

    def __init__(self, directory):
        self.directory = Path(directory) / 'chrome'
        self.directory.mkdir()
        self.proc = None
        self.port = None
        self.failure = None
        self.log = None
        self.targets = set()
        self.token = secrets.token_hex(16)
        # Test threads ask for tabs at the same time: one launches Chrome, the others wait for it.
        self.lock = threading.Lock()

    def launch(self):
        """Starts Chrome and returns its debugging port. Stops it again if it never becomes ready."""
        import headless
        self.log = open(self.directory / 'chrome.log', 'wb')
        headless.adopt_chrome_children()
        self.proc = subprocess.Popen([
            headless.CHROME, '--headless=new', f'--user-data-dir={self.directory}',
            '--remote-debugging-port=0', '--no-first-run', '--no-default-browser-check',
            '--disable-extensions', '--disable-background-timer-throttling',
            '--disable-renderer-backgrounding', '--autoplay-policy=no-user-gesture-required',
            '--enable-webgpu-developer-features', '--enable-logging=stderr', '--v=0',
            '--window-size=1920,1080', *headless.platform_args(),
            *shlex.split(os.environ.get('WRELA_CHROME_ARGS', '')),
            'about:blank'], stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log,
            start_new_session=True)
        try:
            deadline = time.monotonic() + self.STARTUP_SECONDS
            while True:
                try:
                    # Chrome may not have finished writing the file: retry until the deadline.
                    port = int((self.directory / 'DevToolsActivePort').read_text().splitlines()[0])
                    if 0 < port < 65536:
                        return port
                except (FileNotFoundError, IndexError, ValueError):
                    pass
                if self.proc.poll() is not None:
                    raise ValueError(f'it exited with status {self.proc.returncode}')
                if time.monotonic() >= deadline:
                    raise ValueError(f'no debugging port after {self.STARTUP_SECONDS}s')
                time.sleep(.02)
        except BaseException:
            headless.stop(self.proc)
            raise

    def start(self, url):
        if urlparse(url).hostname != '127.0.0.1':
            raise ValueError('test pages must be served on loopback')
        with self.lock:
            if self.failure is not None:
                raise ValueError(self.failure)
            if self.proc is None:
                try:
                    self.port = self.launch()
                except Exception as error:
                    self.failure = f'the test browser failed to start: {error}'
                    raise ValueError(self.failure) from error
            if self.proc.poll() is not None:
                raise ValueError('the test browser exited')
            offset = (self.directory / 'chrome.log').stat().st_size
            req = Request(f'http://127.0.0.1:{self.port}/json/new?{quote(url, safe="")}', method='PUT')
            with urlopen(req, timeout=10) as response:
                target = json.load(response)['id']
            self.targets.add(target)
        return {'target': target, 'log': str(self.directory / 'chrome.log'), 'offset': offset}

    def close(self, target):
        with self.lock:
            if target not in self.targets:
                raise ValueError('unknown test tab')
        with urlopen(f'http://127.0.0.1:{self.port}/json/close/{target}', timeout=10) as response:
            response.read()
        deadline = time.monotonic() + 5
        while True:
            with urlopen(f'http://127.0.0.1:{self.port}/json/list', timeout=5) as response:
                if target not in {item['id'] for item in json.load(response)}:
                    break
            if time.monotonic() >= deadline:
                raise ValueError('the browser did not close the test tab')
            time.sleep(.01)
        with self.lock:
            self.targets.remove(target)

    def __enter__(self):
        session = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                try:
                    if not self.path.startswith('/' + session.token + '/'):
                        self.send_error(404)
                        return
                    payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    action = self.path.rsplit('/', 1)[-1]
                    if action == 'start':
                        result = session.start(payload['url'])
                    elif action == 'close':
                        session.close(payload['target'])
                        result = {}
                    elif action == 'cdp':
                        if payload['target'] not in session.targets:
                            raise ValueError('unknown test tab')
                        result = {'answers': cdp(session.port, payload['target'], payload['commands'])}
                    elif action == 'alive':
                        result = {'alive': session.proc is not None and session.proc.poll() is None}
                    else:
                        raise ValueError('unknown browser action')
                    data = json.dumps(result).encode()
                    self.send_response(200)
                    self.send_header('Content-Length', str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except Exception as error:
                    self.send_error(500, str(error))
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, args=(.05,), daemon=True)
        self.thread.start()
        self.previous = os.environ.get('WRELA_TEST_CHROME')
        os.environ['WRELA_TEST_CHROME'] = f'http://127.0.0.1:{self.server.server_port}/{self.token}'
        return self

    def __exit__(self, *args):
        if self.previous is None:
            os.environ.pop('WRELA_TEST_CHROME', None)
        else:
            os.environ['WRELA_TEST_CHROME'] = self.previous
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        if self.proc is not None:
            import headless
            headless.stop(self.proc)
        if self.log is not None:
            self.log.close()
