"""The reused browser closes each page on success and timeout, isolates logs, and dies on exit."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

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
                                         'WRELA_RESOURCE_DIR': str(root / 'resources'),
                                         'WRELA_CHROME': str(fake), 'WRELA_GPU_SHARED': '2'}), \
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
