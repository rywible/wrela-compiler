"""Release dispatch keeps admission and relative paths in their intended checkouts."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import console
import jobs


class ConsoleAdmission(unittest.TestCase):
    def test_absolute_script_queues_its_checkout_and_preserves_caller_relative_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            caller, checkout = root / "caller", root / "checkout"
            caller.mkdir()
            checkout.mkdir()
            captured = {}

            def dispatch(kind, command):
                captured.update(kind=kind, cwd=Path.cwd(), command=command)
                return 0

            original = Path.cwd()
            try:
                os.chdir(caller)
                arguments = [str(checkout / "tools/console.py"), "--config", "settings.toml",
                             "--output", "dist", "--previous", "old", "--cover", "game=cover.png"]
                with patch.object(sys, "argv", arguments), patch.object(console, "ROOT", checkout), \
                        patch.object(jobs, "admitted", return_value=False), \
                        patch.object(jobs, "run", side_effect=dispatch):
                    self.assertEqual(console.main(), 0)
                self.assertEqual(captured["cwd"], checkout)
                self.assertEqual(captured["kind"], "build")
                options = captured["command"][2:]
                self.assertEqual(options, ["--config", str(caller / "settings.toml"),
                                           "--output", str(caller / "dist"),
                                           "--previous", str(caller / "old"),
                                           "--cover", f"game={caller / 'cover.png'}"])
            finally:
                os.chdir(original)
