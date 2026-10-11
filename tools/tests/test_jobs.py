"""Real process boundaries: global admission, cancellation, renderer slots and storage safety."""
import fcntl
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

TOOLS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS))
import jobs
import resources
import worktrees


def until(predicate, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.02)
    raise AssertionError("process did not reach the expected state")


class Processes(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.env = {**os.environ, "WRELA_RESOURCE_DIR": str(self.root / "resources"),
                    "CARGO_TARGET_DIR": str(self.root / "target")}
        self.env.pop("WRELA_JOB_ID", None)

    def spawn(self, command, **kwargs):
        if command[:2] == [sys.executable, str(TOOLS / "jobs.py")]:
            # Queue fixtures test admission policy separately from this machine's current
            # load. Their tiny fake commands must not wait for unrelated applications.
            bootstrap = f"import sys; sys.path.insert(0,{str(TOOLS)!r}); import jobs; jobs.memory_pressure=lambda:0; sys.argv=['jobs.py',*sys.argv[1:]]; sys.exit(jobs.main())"
            command = [sys.executable, "-c", bootstrap, *command[2:]]
        proc = subprocess.Popen(command, cwd=self.root, env=self.env, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, **kwargs)
        self.addCleanup(self.finish, proc)
        return proc

    def finish(self, proc):
        if proc.poll() is None:
            proc.terminate()
        try:
            proc.communicate(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.communicate()

    def queued(self, script, kind="focused", *options):
        return self.spawn([sys.executable, str(TOOLS / "jobs.py"), "run", kind, *options,
                           "--", sys.executable, "-c", script])

    def holding(self):
        proc = self.queued("from pathlib import Path; import time; Path('ready').touch(); "
                           "\nwhile not Path('release').exists(): time.sleep(.02)")
        until(lambda: (self.root / "ready").exists())
        return proc

    def result(self, proc):
        out, err = proc.communicate(timeout=12)
        return proc.returncode, out + err


class QueueTests(Processes):
    def test_long_tier_preserves_fresh_gate_and_every_stage_with_separate_admissions(self):
        calls = []
        def record(kind, command, **kwargs):
            calls.append((kind, command, kwargs))
            return 0
        with patch.object(jobs, "run", side_effect=record), patch.object(jobs, "fingerprint", return_value={}), \
                patch("ignored.ignored", return_value=[("long", "a"), ("heavy", "b"), ("measure", "c")]), \
                patch.dict(os.environ, {"WRELA_SOFTWARE_GPU": "1"}):
            self.assertEqual(jobs.check(["--long"]), 0)
        self.assertEqual([kind for kind, _, _ in calls], ["gate", *["long"] * 6])
        self.assertIn("--fresh", calls[0][1])
        self.assertEqual([command[-1] for _, command, _ in calls[1:3]], ["a", "b"])
        self.assertEqual([command[-2] for _, command, _ in calls[3:]], ["differential", "gbnf", "baked", "fuzz"])
        self.assertTrue(all(options["verify"] for _, _, options in calls))

    def test_ten_sessions_use_one_job_and_inherit_worker_limits(self):
        first = self.holding()
        script = ("import fcntl,json,os,time; "
                  "f=open('results','a'); fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB); "
                  "f.write(json.dumps({k:os.environ[k] for k in "
                  "['CARGO_BUILD_JOBS','WRELA_CPU_WORKERS','WRELA_TEST_PROGRAMS','CARGO_TARGET_DIR']})+'\\n'); "
                  "f.flush(); time.sleep(.03)")
        pending = []
        for n in range(9):
            temporary = self.root / f"tmp-{n}"
            temporary.mkdir()
            self.env["TMPDIR"] = str(temporary)
            pending.append(self.queued(script))
        until(lambda: len(list((self.root / "resources").glob("job-*.json"))) == 10)
        self.assertFalse((self.root / "results").exists())
        (self.root / "release").touch()
        for proc in [first, *pending]:
            code, output = self.result(proc)
            self.assertEqual(code, 0, output)
        values = [json.loads(line) for line in (self.root / "results").read_text().splitlines()]
        self.assertEqual(len(values), 9)
        for value in values:
            self.assertLessEqual(int(value["CARGO_BUILD_JOBS"]), 4)
            self.assertLessEqual(int(value["WRELA_CPU_WORKERS"]), 2)
            self.assertEqual(value["WRELA_TEST_PROGRAMS"], "1")

    def test_short_work_overtakes_long_work_and_aging_prevents_starvation(self):
        first = self.holding()
        long = self.queued("open('order','a').write('long\\n')", "long")
        until(lambda: len(list((self.root / "resources").glob("job-*.json"))) == 2)
        short = self.queued("open('order','a').write('short\\n')")
        until(lambda: len(list((self.root / "resources").glob("job-*.json"))) == 3)
        (self.root / "release").touch()
        for proc in (first, long, short):
            code, output = self.result(proc)
            self.assertEqual(code, 0, output)
        self.assertEqual((self.root / "order").read_text().splitlines(), ["short", "long"])
        waiting = [dict(id="old", priority=4, created=0, state="queued"),
                   dict(id="new", priority=0, created=300, state="queued")]
        self.assertEqual(jobs.next_job(waiting, 301)["id"], "old")

    def test_new_request_supersedes_only_pending_same_tree_key(self):
        first = self.holding()
        old = self.queued("open('obsolete','w').close()", "background", "--key", "warm")
        until(lambda: len(list((self.root / "resources").glob("job-*.json"))) == 2)
        new = self.queued("open('latest','w').close()", "background", "--key", "warm")
        code, output = self.result(old)
        self.assertEqual(code, 130, output)
        (self.root / "release").touch()
        for proc in (first, new):
            code, output = self.result(proc)
            self.assertEqual(code, 0, output)
        self.assertFalse((self.root / "obsolete").exists())
        self.assertTrue((self.root / "latest").exists())

    def test_failure_and_nested_admission_propagate(self):
        script = f"import subprocess,sys; sys.exit(subprocess.call([sys.executable,{str(TOOLS / 'jobs.py')!r},'run','build','--',sys.executable,'-c','import sys; sys.exit(23)']))"
        code, output = self.result(self.queued(script))
        self.assertEqual(code, 23, output)
        self.assertEqual(output.count("queued "), 1)

    def test_timeout_kills_child_in_a_new_session(self):
        script = "import subprocess,sys,time; p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],start_new_session=True); open('child','w').write(str(p.pid)); time.sleep(60)"
        proc = self.queued(script, "focused", "--timeout", "1")
        until(lambda: (self.root / "child").exists() and bool((self.root / "child").read_text()))
        child = int((self.root / "child").read_text())
        code, output = self.result(proc)
        self.assertEqual(code, 124, output)
        until(lambda: subprocess.run(["ps", "-p", str(child), "-o", "stat="], capture_output=True, text=True).stdout.strip() in ("", "Z"))

    def test_supervisor_death_keeps_child_lease_until_it_exits(self):
        first = self.holding()
        first.kill()
        first.wait(timeout=5)
        second = self.queued("open('second','w').close()")
        time.sleep(.4)
        self.assertFalse((self.root / "second").exists())
        (self.root / "release").touch()
        code, output = self.result(second)
        self.assertEqual(code, 0, output)
        # Recovered metadata from the dead supervisor cannot block subsequent work.
        self.assertFalse(list((self.root / "resources").glob("job-*.json")))

    def test_command_retains_nested_admission_after_supervisor_death(self):
        script = f"from pathlib import Path; import subprocess,sys,time; Path('ready').touch();\nwhile not Path('release').exists(): time.sleep(.02)\nsubprocess.run([sys.executable,{str(TOOLS / 'jobs.py')!r},'run','focused','--',sys.executable,'-c',\"open('nested','w').close()\"],check=True)"
        proc = self.queued(script)
        until(lambda: (self.root / "ready").exists())
        proc.kill()
        proc.wait(timeout=5)
        (self.root / "release").touch()
        until(lambda: (self.root / "nested").exists())

    def test_success_reaps_an_owned_background_preview(self):
        script = "import subprocess,sys,time; p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],start_new_session=True); open('child','w').write(str(p.pid)); time.sleep(.5)"
        proc = self.queued(script)
        until(lambda: (self.root / "child").exists() and bool((self.root / "child").read_text()))
        child = int((self.root / "child").read_text())
        code, output = self.result(proc)
        self.assertEqual(code, 0, output)
        until(lambda: subprocess.run(["ps", "-p", str(child), "-o", "stat="], capture_output=True, text=True).stdout.strip() in ("", "Z"))

    def test_cancel_stops_the_running_job_and_releases_admission(self):
        proc = self.holding()
        path = next((self.root / "resources").glob("job-*.json"))
        identifier = json.loads(path.read_text())["id"]
        cancel = self.spawn([sys.executable, str(TOOLS / "jobs.py"), "cancel", identifier[:8]])
        self.assertEqual(self.result(cancel)[0], 0)
        code, output = self.result(proc)
        self.assertEqual(code, 130, output)
        code, output = self.result(self.queued("pass"))
        self.assertEqual(code, 0, output)

    def test_warning_pressure_holds_admission_until_normal(self):
        script = f"import sys; sys.path.insert(0,{str(TOOLS)!r}); import jobs; from pathlib import Path; jobs.memory_pressure=lambda: 0 if Path('normal').exists() else 1; sys.exit(jobs.run('capture',[sys.executable,'-c',\"open('ran','w').close()\"]))"
        proc = self.spawn([sys.executable, "-c", script])
        until(lambda: bool(list((self.root / "resources").glob("job-*.json"))))
        time.sleep(.3)
        self.assertFalse((self.root / "ran").exists())
        (self.root / "normal").touch()
        code, output = self.result(proc)
        self.assertEqual(code, 0, output)

    def test_warning_pressure_reduces_check_workers_and_critical_stops_admission(self):
        command = "import os,json; print(json.dumps({k:os.environ[k] for k in ['CARGO_BUILD_JOBS','WRELA_CPU_WORKERS','RUST_TEST_THREADS']}))"
        script = f"import sys; sys.path.insert(0,{str(TOOLS)!r}); import jobs; jobs.memory_pressure=lambda:1; sys.exit(jobs.run('focused',[sys.executable,'-c',{command!r}]))"
        code, output = self.result(self.spawn([sys.executable, "-c", script]))
        self.assertEqual(code, 0, output)
        values = json.loads(output.splitlines()[0])
        self.assertEqual(set(values.values()), {"1"})
        self.assertFalse(any(jobs.can_start(kind, 2) for kind in jobs.PRIORITY))
        self.assertFalse(jobs.can_start("measure", 1))

    def test_critical_pressure_stops_owned_work_with_failure(self):
        command = "from pathlib import Path; import time; Path('critical').touch(); time.sleep(60)"
        script = f"import sys; sys.path.insert(0,{str(TOOLS)!r}); import jobs; from pathlib import Path; jobs.memory_pressure=lambda: 2 if Path('critical').exists() else 0; sys.exit(jobs.run('focused',[sys.executable,'-c',{command!r}]))"
        proc = self.spawn([sys.executable, "-c", script])
        code, output = self.result(proc)
        self.assertEqual(code, 75, output)
        self.assertIn("critical memory pressure", output)

    def test_source_mutation_invalidates_a_verification_pass(self):
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=self.root, check=True)
        (self.root / "source").write_text("before")
        (self.root / ".gitignore").write_text("resources/\ntarget/\n")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "initial"], cwd=self.root, check=True)
        code, output = self.result(self.queued("open('source','w').write('after')", "focused", "--verify"))
        self.assertEqual(code, 1, output)
        self.assertIn("source inputs changed", output)


class RendererTests(Processes):
    def test_two_global_slots_and_exclusive_barrier_across_tmpdirs(self):
        def renderer(name, shared):
            return self.spawn([sys.executable, "-c", f"import sys,time; sys.path.insert(0,{str(TOOLS)!r}); from resources import gpu_locks; from pathlib import Path; fds=gpu_locks({shared}); Path({name!r}).touch();\nwhile not Path({(name + '-release')!r}).exists(): time.sleep(.02)"])
        first, second = renderer("a", True), renderer("b", True)
        until(lambda: (self.root / "a").exists() and (self.root / "b").exists())
        self.env["TMPDIR"] = str(self.root / "another-tmp")
        third = renderer("c", True)
        alone = renderer("alone", False)
        time.sleep(.3)
        self.assertFalse((self.root / "c").exists())
        self.assertFalse((self.root / "alone").exists())
        (self.root / "a-release").touch()
        until(lambda: (self.root / "c").exists())
        self.assertFalse((self.root / "alone").exists())
        for name in ("b", "c"):
            (self.root / f"{name}-release").touch()
        until(lambda: (self.root / "alone").exists())
        (self.root / "alone-release").touch()
        for proc in (first, second, third, alone):
            self.assertEqual(self.result(proc)[0], 0)


class StorageTests(Processes):
    def init_repo(self):
        floor = patch.object(worktrees, "MIN_FREE", 0)
        floor.start()
        self.addCleanup(floor.stop)
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=self.root, check=True)
        (self.root / "source").write_text("source")
        (self.root / ".gitignore").write_text("target/\nnode_modules/\n")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "initial"], cwd=self.root, check=True)
        # Each fixture owns its sibling worktree root too.
        siblings = self.root.parent / f".{self.root.name}-sessions"
        self.addCleanup(shutil.rmtree, siblings, True)

    def test_worktrees_share_cache_and_removal_preserves_changes_and_captures(self):
        self.init_repo()
        with patch.dict(os.environ, self.env):
            # Deliberate target overrides are absent for this sharing assertion.
            os.environ.pop("CARGO_TARGET_DIR", None)
            os.environ.pop("WRELA_NATIVE_CACHE", None)
            os.environ.pop("WRELA_CONST_CACHE", None)
            first = worktrees.create(self.root, "one")
            second = worktrees.create(self.root, "two")
            self.assertEqual(worktrees.environment(first), worktrees.environment(second))
            self.assertFalse((first / "target").exists())
            self.assertFalse((first / "node_modules").exists())
            (first / "source").write_text("changed")
            with self.assertRaisesRegex(ValueError, "local changes"):
                worktrees.remove(self.root, "one")
            (first / "source").write_text("source")
            (first / "target").mkdir()
            (first / "target/capture.png").write_bytes(b"evidence")
            with self.assertRaisesRegex(ValueError, "preserve ignored output"):
                worktrees.remove(self.root, "one")
            self.assertEqual((first / "target/capture.png").read_bytes(), b"evidence")
            worktrees.remove(self.root, "two")
            self.assertFalse(second.exists())

    def test_removal_refuses_unmerged_commits_and_live_jobs(self):
        self.init_repo()
        with patch.dict(os.environ, self.env):
            tree = worktrees.create(self.root, "busy")
            lease_path = resources.resource_dir() / "job-test.lease"
            with open(lease_path, "a") as lease:
                fcntl.flock(lease, fcntl.LOCK_EX)
                record = lease_path.with_suffix(".json")
                record.write_text(json.dumps({"cwd": str(tree), "id": "test", "state": "queued"}))
                with self.assertRaisesRegex(ValueError, "queued or running"):
                    worktrees.remove(self.root, "busy")
                record.unlink()
            (tree / "source").write_text("new commit")
            subprocess.run(["git", "add", "source"], cwd=tree, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "unmerged"], cwd=tree, check=True)
            with self.assertRaisesRegex(ValueError, "commits not in main"):
                worktrees.remove(self.root, "busy")

    def test_scratch_pruning_requires_old_age_and_dead_owner(self):
        for name, pid in (("live", os.getpid()), ("dead", 2147483647), ("unknown", None)):
            path = self.root / f"test-scratch-{name}"
            path.mkdir()
            if pid:
                (path / ".owner").write_text(str(pid))
            old = time.time() - 7 * 3600
            os.utime(path, (old, old))
        worktrees.prune_scratch(self.root)
        self.assertFalse((self.root / "test-scratch-dead").exists())
        self.assertTrue((self.root / "test-scratch-live").exists())
        self.assertTrue((self.root / "test-scratch-unknown").exists())

    def test_low_disk_guard_and_build_prune_leave_sources_and_captures(self):
        self.init_repo()
        cache = self.root / ".git/wrela/cache/cargo"
        cache.mkdir(parents=True)
        (cache / "disposable").write_bytes(b"build")
        (self.root / "target").mkdir()
        evidence = self.root / "target/capture.png"
        evidence.write_bytes(b"pixels")
        with patch.dict(os.environ, self.env), patch.object(worktrees.shutil, "disk_usage", return_value=type("Disk", (), {"free": 2 * worktrees.GIB})()):
            with patch.object(worktrees, "MIN_FREE", 12 * worktrees.GIB):
                with self.assertRaisesRegex(ValueError, "12 GiB"):
                    worktrees.maintain(self.root)
        with patch.dict(os.environ, self.env):
            worktrees.maintain(self.root, clear_build=True)
        self.assertFalse((cache / "disposable").exists())
        self.assertEqual(evidence.read_bytes(), b"pixels")
        self.assertEqual((self.root / "source").read_text(), "source")


if __name__ == "__main__":
    unittest.main()
