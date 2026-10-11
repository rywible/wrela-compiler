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
    def setUp(self):
        super().setUp()
        self.write("compiler/tests/tests/suite/herd.rs", '#[test]\n#[ignore = "measure: alone"]\n'
                   "fn the_herd_keeps_its_frames() {}\n")

    def test_a_test_run_with_cargo_is_refused(self):
        for command in [
            "cargo test --release -p wrela-tests --test suite clearing::no_field 2>&1 | tail",
            "cd /x && cargo test -q --release -p wrela-tests --test suite -- --ignored --exact a",
            "for t in a b; do cargo test -q --release --workspace -- --ignored --exact $t; done",
            "(time cargo test --release -p wrela-tests --test suite x) > $S/x.log 2>&1",
        ]:
            self.assertTrue(hooks.takes_the_gpu_alone(command, self.root), command)

    def test_measurements_and_runs_check_sh_cant_make_go_through(self):
        for command in [
            "cargo test --release --workspace -- --ignored --exact herd::the_herd_keeps_its_frames",
            "WRELA_GPU_SHARED=1 cargo test --release -p wrela-tests --test suite clearing::x",
            "WRELA_BLESS=1 cargo test -q --release -p wrela-tests --test suite diagnostics::",
            "WRELA_RUN_ONLY=jobs cargo test -q --release -p wrela-tests --test suite -- run_pass",
            "cargo test -q --release --workspace --no-run --message-format=json > t.json",
            "cargo test -q --release --workspace --doc",
            'grep -rn "cargo test" tools/check.sh',
            "tools/check.sh clearing:: --nocapture",
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
        self.assertIn("tools/check.sh <name> --nocapture", out.stderr)
        self.assertEqual(out.stdout, "")


class GateTest(Checkout, unittest.TestCase):
    def test_what_runs_the_gate(self):
        for command in [
            "tools/check.sh",
            "tools/check.sh > $S/gate.log 2>&1; echo \"exit $?\" >> $S/gate.log",
            "./tools/check.sh 2>&1 | tail -3",
            "cargo fmt --all && tools/check.sh --long > $S/long.txt 2>&1",
            "(tools/check.sh > $S/gate.log 2>&1; echo done)",
            "cd /Users/x/wrela && /Users/x/wrela/tools/check.sh --full",
        ]:
            self.assertTrue(hooks.runs_gate(command), command)
        for command in [
            "tools/check.sh clearing:: lock",
            "tools/check.sh --long clearing:: --nocapture",
            "tools/check.sh --fmt",
            "sed -n 1,60p tools/check.sh",
            'grep -n "budget" tools/check.sh',
        ]:
            self.assertFalse(hooks.runs_gate(command), command)

    def test_the_gate_is_formatted_first_and_the_files_named(self):
        # check.sh --fmt prints what it formatted: rustfmt absolute paths, wrela fmt relative.
        self.write("tools/check.sh", f'#!/bin/sh\n[ "$1" = --fmt ] || exit 9\n'
                   f'echo "{self.root}/compiler/a.rs"\necho engine/b.wrela\n', 0o755)
        out = run("bash", {"cwd": self.root, "tool_input": {"command": "tools/check.sh"}})
        self.assertEqual(out.returncode, 0, out.stderr)
        said = json.loads(out.stdout)
        self.assertEqual(said["systemMessage"],
                         "Formatted before the gate: compiler/a.rs, engine/b.wrela")
        self.assertEqual(said["hookSpecificOutput"]["hookEventName"], "PreToolUse")
        self.assertIn("compiler/a.rs, engine/b.wrela",
                      said["hookSpecificOutput"]["additionalContext"])

    def test_nothing_formatted_says_nothing(self):
        self.write("tools/check.sh", "#!/bin/sh\nexit 0\n", 0o755)
        out = run("bash", {"cwd": self.root, "tool_input": {"command": "tools/check.sh --long"}})
        self.assertEqual((out.returncode, out.stdout), (0, ""))

    def test_a_formatter_that_fails_lets_the_gate_run(self):
        out = run("bash", {"cwd": self.root, "tool_input": {"command": "tools/check.sh"}})
        self.assertEqual((out.returncode, out.stdout), (1, ""))  # no check.sh: an error, not "no"
        self.assertIn("tools/hooks.py bash", out.stderr)


class SessionTest(Checkout, unittest.TestCase):
    def test_no_task_file_says_nothing(self):
        out = run("session", {"cwd": self.root, "source": "startup"})
        self.assertEqual((out.returncode, out.stdout), (0, ""))

    def test_a_new_session_is_told_where_the_task_file_is(self):
        self.write("M7-TASKS.md", "# M7\n\n## Status\n\n| AC1 | done |\n")
        for source in ("startup", "resume", "clear"):
            out = run("session", {"cwd": self.root, "source": source})
            self.assertEqual(out.stdout, "M7-TASKS.md is the open milestone's task file: read it "
                             "before milestone work.\n")

    def test_after_a_compaction_the_task_file_is_given(self):
        text = "# M7\n\n## Status\n\n| AC1 | done |\n"
        self.write("M7-TASKS.md", text)
        out = run("session", {"cwd": self.root, "source": "compact"})
        self.assertIn("M7-TASKS.md, as it is now", out.stdout)
        self.assertIn(text, out.stdout)

    def test_a_long_task_file_is_cut_at_a_heading(self):
        head = "# M7\n\n## Status\n\n" + "| AC | state |\n" * 200
        self.write("M7-TASKS.md", head + "\n## Decisions\n\n" + "a decision\n" * 1000)
        out = run("session", {"cwd": self.root, "source": "compact"}).stdout
        self.assertLess(len(out), 10000)
        self.assertIn(head, out)
        self.assertNotIn("## Decisions", out)
        self.assertTrue(out.endswith("(The rest is in M7-TASKS.md.)\n"))


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

    def session(self, source):
        out = run("session", {"cwd": self.root, "source": source},
                  {"CLAUDE_CODE_REMOTE": "true", "CLAUDE_ENV_FILE": self.env_file})
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
