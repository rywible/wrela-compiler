"""Tests for tools/hooks.py, the Claude Code hooks: what each refuses and lets through, and what it
says.

    python3 -m unittest discover -s tools/tests
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

TESTS = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.dirname(TESTS)
sys.path.insert(0, TOOLS)

import hooks  # noqa: E402


def run(hook, event, env=None):
    """Runs a hook as Claude Code does: the event's JSON on stdin; on a machine of one's own (not
    a cloud machine) unless `env` says otherwise."""
    base = {k: v for k, v in os.environ.items() if k not in ("CLAUDE_CODE_REMOTE", "CLAUDE_ENV_FILE")}
    return subprocess.run([sys.executable, os.path.join(TOOLS, "hooks.py"), hook],
                          input=json.dumps(event), capture_output=True, text=True, timeout=60,
                          env={**base, **(env or {})})


class Checkout:
    """Mixin: each test gets a git checkout of its own, `root`, removed afterwards."""

    def setUp(self):
        super().setUp()
        self.root = os.path.realpath(tempfile.mkdtemp())
        subprocess.run(["git", "init", "-q", self.root], check=True)

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)
        super().tearDown()

    def write(self, path, text, mode=0o644):
        path = os.path.join(self.root, path)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
        os.chmod(path, mode)


class PollTest(unittest.TestCase):
    def test_a_loop_of_sleep_in_the_foreground_is_refused(self):
        for command in [
            'until grep -q "^exit" $S/long.txt; do sleep 30; done; echo finished',
            "while pgrep -f wrela-host >/dev/null; do sleep 2; done",
            "sleep 60; tail -4 $S/long.log",
        ]:
            self.assertTrue(hooks.polls(command, background=False), command)
            self.assertFalse(hooks.polls(command, background=True), command)

    def test_other_sleeps_and_loops_go_through(self):
        for command in [
            "sleep 1; cat $S/out.txt",
            '( "$@" ) & pid=$!; ( sleep 300; kill $pid 2>/dev/null ) & w=$!; wait $pid',
            "while read -r f; do wc -l $f; done < files.txt",
            "for i in 1 2 3; do tools/check.sh lock; done",
        ]:
            self.assertFalse(hooks.polls(command, background=False), command)


class CargoTestTest(Checkout, unittest.TestCase):
    def test_raw_builds_are_refused_and_scheduled_builds_are_allowed(self):
        for command in ("cargo build --release", "cargo check -p wrela-syntax", "cargo test --no-run"):
            out = run("bash", {"cwd": self.root, "tool_input": {"command": command}})
            self.assertEqual(out.returncode, 2, out.stderr)
            self.assertIn("tools/jobs.py", out.stderr)
        out = run("bash", {"cwd": self.root, "tool_input": {"command": "python3 tools/jobs.py run build -- cargo build --release"}})
        self.assertEqual(out.returncode, 0, out.stderr)
    def test_a_test_run_with_cargo_is_refused(self):
        for command in [
            "cargo test --release -p wrela-tests --test suite clearing::no_field 2>&1 | tail",
            "cd /x && cargo test -q --release -p wrela-tests --test suite -- --ignored --exact a",
            "for t in a b; do cargo test -q --release --workspace -- --ignored --exact $t; done",
            "(time cargo test --release -p wrela-tests --test suite x) > $S/x.log 2>&1",
        ]:
            self.assertTrue(hooks.takes_the_gpu_alone(command, self.root), command)

    def test_compile_only_and_non_test_cargo_commands_go_through(self):
        for command in [
                        "WRELA_GPU_SHARED=1 cargo test --release -p wrela-tests --test suite clearing::x",
            "WRELA_BLESS=1 cargo test -q --release -p wrela-tests --test suite diagnostics::",
            "WRELA_RUN_ONLY=jobs cargo test -q --release -p wrela-tests --test suite -- run_pass",
            "cargo test -q --release --workspace --no-run --message-format=json > t.json",
            "cargo test -q --release --workspace --doc",
            'grep -rn "cargo test" tools/check.sh',
            "tools/check.sh clearing::the_walks_own_tests_pass",
        ]:
            self.assertFalse(hooks.takes_the_gpu_alone(command, self.root), command)

    def test_text_a_command_only_quotes_runs_nothing(self):
        for command in [
            "python3 - <<'EOF'\ns = 'a test run with `cargo test` takes the GPU'\nEOF\necho done",
            'git commit -q -F - <<EOF\nRun `cargo test --release clearing::x` alone.\nEOF',
            "git commit -m 'Run (cargo test clearing::x) alone'",
            "cat > x.md <<-END\n\tcargo test -p a\n\tEND\n",
        ]:
            self.assertFalse(hooks.takes_the_gpu_alone(hooks.code(command), self.root), command)
        after = "python3 - <<'EOF'\nprint(1)\nEOF\ncargo test -p wrela-tests clearing::y"
        self.assertTrue(hooks.takes_the_gpu_alone(hooks.code(after), self.root))
        out = run("bash", {"cwd": self.root, "tool_input": {"command": after.replace("y", "z")}})
        self.assertEqual(out.returncode, 2)
        out = run("bash", {"cwd": self.root, "tool_input": {
            "command": "git commit -F - <<'EOF'\nNot `cargo test`: `tools/check.sh`.\nEOF"}})
        self.assertEqual((out.returncode, out.stdout, out.stderr), (0, "", ""))

    def test_the_refusal_says_what_to_run_instead(self):
        out = run("bash", {"cwd": self.root,
                           "tool_input": {"command": "cargo test -p wrela-tests clearing::x"}})
        self.assertEqual(out.returncode, 2)
        self.assertIn("tools/check.sh <name>", out.stderr)
        self.assertEqual(out.stdout, "")


class CheckHookTest(Checkout, unittest.TestCase):
    def test_checks_do_not_format_or_run_other_commands_in_a_hook(self):
        self.write("tools/check.sh", "#!/bin/sh\nexit 9\n", 0o755)
        for command in ("tools/check.sh", "tools/check.sh renderer::agreement"):
            out = run("bash", {"cwd": self.root, "tool_input": {"command": command}})
            self.assertEqual((out.returncode, out.stdout, out.stderr), (0, "", ""))


class SessionTest(Checkout, unittest.TestCase):
    def test_no_task_file_says_nothing(self):
        out = run("session", {"cwd": self.root, "source": "startup"})
        self.assertEqual((out.returncode, out.stdout), (0, ""))

    def test_a_new_session_is_told_where_the_task_file_is(self):
        self.write("TASKS.md", "# The open work\n\n## Status\n\n| AC1 | done |\n")
        for source in ("startup", "resume", "clear"):
            out = run("session", {"cwd": self.root, "source": source})
            self.assertEqual(out.stdout, "TASKS.md has the open work: read it before you start.\n")

    def test_after_a_compaction_the_task_file_is_given(self):
        text = "# The open work\n\n## Status\n\n| AC1 | done |\n"
        self.write("TASKS.md", text)
        out = run("session", {"cwd": self.root, "source": "compact"})
        self.assertIn("The open work, TASKS.md, as it is now", out.stdout)
        self.assertIn(text, out.stdout)

    def test_a_long_task_file_is_cut_at_a_heading(self):
        head = "# The open work\n\n## Status\n\n" + "| AC | state |\n" * 200
        self.write("TASKS.md", head + "\n## Decisions\n\n" + "a decision\n" * 1000)
        out = run("session", {"cwd": self.root, "source": "compact"}).stdout
        self.assertLess(len(out), 10000)
        self.assertIn(head, out)
        self.assertNotIn("## Decisions", out)
        self.assertTrue(out.endswith("(The rest is in TASKS.md.)\n"))


class CloudTest(Checkout, unittest.TestCase):
    """A session on a cloud machine, with a stand-in for tools/cloud.sh that notes each run."""

    def setUp(self):
        super().setUp()
        self.runs = os.path.join(self.root, "runs")
        self.env_file = os.path.join(self.root, "env")
        self.write("tools/cloud.sh", f"""#!/bin/sh
echo "$1" >> {self.runs}
case "$1" in
  --env) echo WRELA_SOFTWARE_GPU=1; echo "WRELA_CHROME_ARGS=--a --b" ;;
  --check) [ -e {self.root}/provisioned ] ;;
esac
""", 0o755)

    def session(self, source, shared="no/such/folder"):
        out = run("session", {"cwd": self.root, "source": source},
                  {"CLAUDE_CODE_REMOTE": "true", "CLAUDE_ENV_FILE": self.env_file,
                   "WRELA_SHARED_FOLDER": os.path.join(self.root, shared)})
        self.assertEqual(out.returncode, 0, out.stderr)
        return out.stdout

    def ran(self, warm):
        """The stand-in's runs, once `--warm` (which runs in the background) has, if `warm`."""
        for _ in range(100):
            with open(self.runs) as f:
                lines = f.read().split()
            if not warm or "--warm" in lines:
                return lines
            time.sleep(0.05)
        return lines

    def test_a_new_session_is_told_and_its_builds_start(self):
        open(os.path.join(self.root, "provisioned"), "w").close()
        out = self.session("startup")
        self.assertIn("Its GPU is the CPU", out)
        self.assertIn("building in the background", out)
        self.assertNotIn("provisioned now", out)
        self.assertEqual(sorted(self.ran(True)), ["--check", "--env", "--warm"])
        with open(self.env_file) as f:
            self.assertEqual(f.read(), "export WRELA_SOFTWARE_GPU=1\nexport WRELA_CHROME_ARGS='--a --b'\n")

    def test_in_a_project_the_constants_cache_is_in_its_shared_folder(self):
        open(os.path.join(self.root, "provisioned"), "w").close()
        os.mkdir(os.path.join(self.root, "shared"))
        self.session("resume", shared="shared")
        with open(self.env_file) as f:
            cache = os.path.join(self.root, "shared", ".wrela-consts")
            self.assertIn(f"export WRELA_CONST_CACHE={cache}\n", f.read())

    def test_a_machine_not_provisioned_is_provisioned_in_the_background(self):
        for source in ("startup", "resume"):
            if os.path.exists(self.runs):
                os.remove(self.runs)
            out = self.session(source)
            self.assertIn("run `tools/cloud.sh` before anything that builds or renders", out)
            self.assertIn("--warm", self.ran(True))

    def test_a_resumed_session_on_a_provisioned_machine_builds_nothing(self):
        open(os.path.join(self.root, "provisioned"), "w").close()
        out = self.session("resume")
        self.assertIn("Its GPU is the CPU", out)
        self.assertNotIn("background", out)
        self.assertEqual(self.ran(False), ["--env", "--check"])

    def test_a_machine_of_ones_own_is_left_alone(self):
        out = run("session", {"cwd": self.root, "source": "startup"})
        self.assertEqual((out.returncode, out.stdout), (0, ""))
        self.assertFalse(os.path.exists(self.runs))


class MainTest(unittest.TestCase):
    def test_a_broken_event_lets_the_command_run(self):
        out = subprocess.run([sys.executable, os.path.join(TOOLS, "hooks.py"), "bash"],
                             input="not json", capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 1)  # 2 would refuse the command

    def test_usage(self):
        out = subprocess.run([sys.executable, os.path.join(TOOLS, "hooks.py"), "other"],
                             input="{}", capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 1)
        self.assertIn("bash | session", out.stderr)


if __name__ == "__main__":
    unittest.main()
