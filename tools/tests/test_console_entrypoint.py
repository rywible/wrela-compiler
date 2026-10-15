"""Release commands preserve caller-relative paths and propagate build failures."""
import contextlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import console


class ConsoleEntrypoint(unittest.TestCase):
    def test_direct_release_preserves_caller_relative_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            caller, checkout = root / "caller", root / "checkout"
            caller.mkdir()
            checkout.mkdir()
            original = Path.cwd()
            try:
                os.chdir(caller)
                arguments = ["console.py", "--config", "settings.toml", "--output", "dist",
                             "--previous", "old"]
                with patch.object(sys, "argv", arguments), patch.object(console, "ROOT", checkout), \
                        patch.object(console, "read_config", return_value={}) as read, \
                        patch.object(console, "package", return_value={}) as package, \
                        contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(console.main(), 0)
                self.assertEqual(Path.cwd(), checkout)
                read.assert_called_once_with(caller / "settings.toml")
                package.assert_called_once_with({}, caller / "dist", caller / "old")
            finally:
                os.chdir(original)

    def test_build_failure_returns_failure_instead_of_a_release(self):
        original = Path.cwd()
        output = io.StringIO()
        try:
            with patch.object(sys, "argv", ["console.py", "--output", "dist"]), \
                    patch.object(console, "read_config", return_value={}), \
                    patch.object(console, "package", side_effect=subprocess.CalledProcessError(7, ["cargo"])), \
                    contextlib.redirect_stderr(output):
                self.assertEqual(console.main(), 1)
            self.assertIn("console release:", output.getvalue())
        finally:
            os.chdir(original)
