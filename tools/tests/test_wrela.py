"""The CLI wrapper builds its own checkout and forwards command failures."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class DirectCli(unittest.TestCase):
    def test_checkout_local_build_arguments_and_exit_statuses(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout, caller, binaries = root / "checkout", root / "caller", root / "bin"
            (checkout / "tools").mkdir(parents=True)
            caller.mkdir()
            binaries.mkdir()
            wrapper = checkout / "tools/wrela"
            shutil.copyfile(Path(__file__).parents[1] / "wrela", wrapper)
            cargo = binaries / "cargo"
            cargo.write_text(f"#!{sys.executable}\n" + '''
import json, os, pathlib, sys
pathlib.Path(os.environ['BUILD_RECORD']).write_text(json.dumps({'cwd': os.getcwd(), 'args': sys.argv[1:]}))
sys.exit(int(os.environ.get('BUILD_STATUS', '0')))
''')
            cargo.chmod(0o755)
            for custom in (False, True):
                target = root / "custom" if custom else checkout / "target"
                executable = target / "release/wrela"
                executable.parent.mkdir(parents=True)
                executable.write_text(f"#!{sys.executable}\n" + '''
import json, os, pathlib, sys
pathlib.Path(os.environ['CLI_RECORD']).write_text(json.dumps(sys.argv[1:]))
sys.exit(int(os.environ.get('CLI_STATUS', '0')))
''')
                executable.chmod(0o755)
                env = {**os.environ, "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
                       "BUILD_RECORD": str(root / "build"), "CLI_RECORD": str(root / "cli")}
                env.pop("CARGO_TARGET_DIR", None)
                if custom:
                    env["CARGO_TARGET_DIR"] = str(target)
                for build_status, cli_status, expected in ((0, 0, 0), (0, 9, 9), (7, 0, 7)):
                    with self.subTest(custom=custom, build=build_status, cli=cli_status):
                        (root / "cli").unlink(missing_ok=True)
                        env.update(BUILD_STATUS=str(build_status), CLI_STATUS=str(cli_status))
                        result = subprocess.run([sys.executable, str(wrapper), "query", "hello world"],
                                                cwd=caller, env=env, capture_output=True, text=True)
                        self.assertEqual(result.returncode, expected, result.stderr)
                        build = json.loads((root / "build").read_text())
                        self.assertEqual(build, {"cwd": str(checkout), "args": [
                            "build", "-q", "--release", "-p", "wrela", "-p", "wrela-host", "--bins"]})
                        if build_status:
                            self.assertFalse((root / "cli").exists())
                        else:
                            self.assertEqual(json.loads((root / "cli").read_text()), ["query", "hello world"])
