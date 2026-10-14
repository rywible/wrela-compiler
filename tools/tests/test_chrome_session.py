"""The reused browser closes each page on success and timeout, isolates logs, and dies on exit."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
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
        self.assertEqual(headless.platform_args('darwin'), [])
