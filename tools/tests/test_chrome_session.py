"""The reused browser closes each page on success and timeout, isolates logs, and dies on exit."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chrome_session
import headless
from test_tools import PageDir


class Session(PageDir, unittest.TestCase):
    def test_fresh_tabs_timeout_cleanup_console_isolation_and_process_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fake = Path(__file__).with_name('fake_cdp.py')
            with patch.dict(os.environ, {'TMPDIR': temporary,
                                         'WRELA_CHROME': str(fake)}), \
                    patch.object(headless, 'CHROME', str(fake)):
                previous = os.environ.get('WRELA_TEST_CHROME')
                with chrome_session.ChromeSession(root) as session:
                    self.assertIsNone(session.proc)  # starting a CPU check needs no browser
                    def run(fragment):
                        return subprocess.run([sys.executable, str(Path(headless.__file__)),
                                               self.page, fragment, '.15' if fragment == '#timeout' else '2'],
                                              capture_output=True, text=True, timeout=10)
                    first = run('#run')
                    self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
                    proc = session.proc
                    second = run('#timeout')
                    self.assertEqual(second.returncode, 1, second.stdout + second.stderr)
                    self.assertIn('timed out', second.stderr)
                    self.assertFalse(session.targets)
                    third = run('#run')
                    self.assertEqual(third.returncode, 0, third.stdout + third.stderr)
                    self.assertIs(session.proc, proc)
                    console = (Path(self.page_dir) / 'results/console.log').read_text()
                    self.assertIn('tab 3', console)
                    self.assertNotIn('tab 1', console)
                    self.assertNotIn('tab 2', console)
                    self.assertFalse(session.targets)
                    proc.kill(); proc.wait()
                    self.assertFalse(chrome_session.request(os.environ['WRELA_TEST_CHROME'], 'alive', {})['alive'])
                self.assertEqual(os.environ.get('WRELA_TEST_CHROME'), previous)
                self.assertIsNotNone(proc.poll())


    def test_overlapping_scenarios_share_one_chrome_and_keep_their_own_console(self):
        other = tempfile.mkdtemp(prefix='.test-page-', dir=Path(self.page_dir).parent)
        self.addCleanup(shutil.rmtree, other, True)
        with tempfile.TemporaryDirectory() as temporary:
            fake = Path(__file__).with_name('fake_cdp.py')
            with patch.dict(os.environ, {'TMPDIR': temporary, 'WRELA_CHROME': str(fake),
                                         'FAKE_CDP_DELAY': '.3'}), \
                    patch.object(headless, 'CHROME', str(fake)), \
                    chrome_session.ChromeSession(Path(temporary)) as session:
                # Both ask for a tab while Chrome is still starting; the first stays open while
                # the second's page logs.
                runs = [subprocess.Popen([sys.executable, str(Path(headless.__file__)), page, fragment, limit],
                                         stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
                        for page, fragment, limit in [(self.page, '#timeout', '1.5'),
                                                      (os.path.relpath(other, headless.serve.ROOT), '#run', '5')]]
                outputs = [run.communicate(timeout=20)[0] for run in runs]
                self.assertEqual([run.returncode for run in runs], [1, 0], outputs)
                self.assertIn('timed out', outputs[0])
                self.assertEqual(len((session.directory / 'launches').read_text().splitlines()), 1)
                self.assertFalse(session.targets)
                consoles = [(Path(page) / 'results/console.log').read_text().splitlines()
                            for page in (self.page_dir, other)]
                self.assertEqual([len(lines) for lines in consoles], [1, 1], consoles)
                self.assertNotEqual(consoles[0], consoles[1])
                self.assertTrue(all(line.startswith('tab ') for line, in consoles))

    def test_a_page_that_finishes_before_its_devtools_input_does_waits_for_it(self):
        def slow(port, target, commands, fail):
            time.sleep(.5)
            if fail:
                raise ValueError('the page never had ready')
            return []
        with tempfile.TemporaryDirectory() as temporary:
            fake = Path(__file__).with_name('fake_cdp.py')
            commands = Path(temporary) / 'commands.json'
            commands.write_text('[{"until": "ready"}]')
            for fail, status in [(True, 1), (False, 0)]:
                with self.subTest(fail=fail), patch.dict(os.environ, {'WRELA_CHROME': str(fake)}), \
                        patch.object(headless, 'CHROME', str(fake)), \
                        patch.object(chrome_session, 'cdp', lambda *a: slow(*a, fail)), \
                        chrome_session.ChromeSession(Path(tempfile.mkdtemp(dir=temporary))):
                    # The fake page is done at once; the run is the input's too.
                    run = subprocess.run([sys.executable, str(Path(headless.__file__)), '--cdp',
                                          str(commands), self.page, '#run', '5'],
                                         capture_output=True, text=True, timeout=20)
                    self.assertEqual(run.returncode, status, run.stdout + run.stderr)
                    if fail:
                        self.assertIn('the page never had ready', run.stderr)

    def test_a_failed_start_fails_every_waiting_scenario_and_stops_the_browser(self):
        for mode in ['exit', 'hang', 'garbage']:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                fake = Path(__file__).with_name('fake_cdp.py')
                with patch.dict(os.environ, {'WRELA_CHROME': str(fake), 'FAKE_CDP_MODE': mode}), \
                        patch.object(headless, 'CHROME', str(fake)), \
                        patch.object(chrome_session.ChromeSession, 'STARTUP_SECONDS', .5), \
                        chrome_session.ChromeSession(Path(temporary)) as session:
                    endpoint = os.environ['WRELA_TEST_CHROME']
                    errors = []

                    def start():
                        try:
                            chrome_session.request(endpoint, 'start', {'url': 'http://127.0.0.1:1/p/#timeout'})
                        except HTTPError as error:
                            errors.append(error.reason)
                    threads = [threading.Thread(target=start) for _ in range(3)]
                    for thread in threads:
                        thread.start()
                    for thread in threads:
                        thread.join(10)
                    start()  # a later scenario gets the same answer and no second browser
                    self.assertEqual(len(errors), 4, errors)
                    self.assertEqual(len(set(errors)), 1, errors)
                    self.assertIn('the test browser failed to start', errors[0])
                    self.assertNotIn('None', errors[0])
                    self.assertEqual(len((session.directory / 'launches').read_text().splitlines()), 1)
                    self.assertIsNotNone(session.proc.poll())
                    self.assertFalse(chrome_session.request(endpoint, 'alive', {})['alive'])


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


class DevTools(unittest.TestCase):
    """The DevTools client (`chrome_session.cdp`) against a WebSocket server of the test's own."""

    @staticmethod
    def serve(answer):
        import base64, hashlib, json, socket, struct
        listener = socket.socket()
        listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        seen = []

        def frame(data, opcode=1):
            head = bytes([0x80 | opcode])
            n = len(data)
            head += bytes([n]) if n < 126 else bytes([126]) + struct.pack('>H', n)
            return head + data

        def run():
            conn, _ = listener.accept()
            with conn:
                head = b''
                while b'\r\n\r\n' not in head:
                    head += conn.recv(4096)
                key = [l.split(b': ')[1] for l in head.split(b'\r\n') if l.startswith(b'Sec-WebSocket-Key')][0]
                accept = base64.b64encode(hashlib.sha1(key + chrome_session.Socket.GUID.encode()).digest())
                conn.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
                             b'Sec-WebSocket-Accept: ' + accept + b'\r\n\r\n')
                buffer = b''
                def read(n):
                    nonlocal buffer
                    while len(buffer) < n:
                        chunk = conn.recv(4096)
                        if not chunk:
                            raise EOFError
                        buffer += chunk
                    out, buffer = buffer[:n], buffer[n:]
                    return out
                try:
                    while True:
                        first, second = read(2)
                        n = second & 0x7f
                        if n == 126:
                            n = struct.unpack('>H', read(2))[0]
                        assert second & 0x80, 'a client masks its frames'
                        mask = read(4)
                        data = bytes(b ^ mask[i % 4] for i, b in enumerate(read(n)))
                        if first & 0x0f == 10:
                            seen.append('pong')
                            continue
                        message = json.loads(data)
                        seen.append(message['method'])
                        # A ping and an event that answers nothing come before each answer.
                        conn.sendall(frame(b'hi', opcode=9))
                        conn.sendall(frame(json.dumps({'method': 'Page.event'}).encode()))
                        conn.sendall(frame(json.dumps({'id': message['id'], **answer(message)}).encode()))
                except EOFError:
                    pass
        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        return listener.getsockname()[1], seen, thread

    def test_commands_run_in_order_and_a_page_is_waited_for(self):
        evaluations = []
        def answer(message):
            if message['method'] == 'Runtime.evaluate':
                evaluations.append(1)
                return {'result': {'result': {'value': len(evaluations) >= 2}}}
            return {'result': {'ok': message['method']}}
        port, seen, thread = self.serve(answer)
        answers = chrome_session.cdp(port, 'tab', [
            {'until': 'ready'}, {'wait': 0}, {'method': 'Input.insertText', 'params': {'text': 'か'}}])
        thread.join(5)
        self.assertEqual(answers, [{'ok': 'Input.insertText'}])
        self.assertEqual([s for s in seen if s != 'pong'], ['Runtime.evaluate', 'Runtime.evaluate', 'Input.insertText'])
        self.assertIn('pong', seen)

    def test_an_error_or_a_page_never_ready_says_why(self):
        port, _, thread = self.serve(lambda m: {'error': {'message': 'no such key'}})
        with self.assertRaisesRegex(ValueError, 'Input.dispatchKeyEvent: no such key'):
            chrome_session.cdp(port, 'tab', [{'method': 'Input.dispatchKeyEvent'}])
        thread.join(5)
        port, _, thread = self.serve(lambda m: {'result': {'result': {'value': False}}})
        with self.assertRaisesRegex(ValueError, 'never had ready'):
            chrome_session.cdp(port, 'tab', [{'until': 'ready', 'timeout': 0.2}])
        thread.join(5)
