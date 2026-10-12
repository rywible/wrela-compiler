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
    def test_check_uses_one_verified_admission_and_rejects_modes(self):
        with patch.object(jobs, "run", return_value=0) as run:
            self.assertEqual(jobs.check([]), 0)
            self.assertEqual(run.call_args.args[0], "gate")
            self.assertTrue(run.call_args.kwargs["verify"])
            self.assertEqual(jobs.check(["renderer::agreement"]), 0)
            self.assertEqual(run.call_args.args[0], "focused")
            self.assertEqual(run.call_args.args[1][-1], "renderer::agreement")
            for args in (["--long"], ["--full"], ["--fresh"], ["a", "b"]):
                with self.assertRaises(ValueError): jobs.check(args)
            self.assertEqual(run.call_count, 2)

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

    def cargo_fixture(self, root, value):
        (root / "src").mkdir(exist_ok=True)
        (root / "Cargo.toml").write_text(
            '[package]\nname = "cache_fixture"\nversion = "0.1.0"\nedition = "2024"\n[workspace]\n')
        (root / "Cargo.lock").write_text(
            '# This file is automatically @generated by Cargo.\n'
            '# It is not intended for manual editing.\nversion = 4\n\n'
            '[[package]]\nname = "cache_fixture"\nversion = "0.1.0"\n')
        (root / "src/main.rs").write_text('fn main() { print!("{}", include_str!("value.txt")); }\n')
        (root / "src/value.txt").write_text(value)

    def test_shared_cargo_rebuilds_older_checkout_content_and_keeps_dependencies(self):
        self.init_repo()
        first = worktrees.create(self.root, "cache-first")
        second = worktrees.create(self.root, "cache-second")
        self.cargo_fixture(first, "first")
        self.cargo_fixture(second, "second")
        target = self.root / "target"
        dependency = target / "debug/.fingerprint/serde-0000000000000000"
        dependency.mkdir(parents=True)
        (dependency / "lib-serde").write_text("preserve dependency freshness")

        def build(root):
            subprocess.run(["cargo", "build", "--quiet"], cwd=root,
                           env={**self.env, "CARGO_TARGET_DIR": str(target), "CARGO_BUILD_JOBS": "1"},
                           check=True, capture_output=True, text=True)
            return subprocess.check_output([str(target / "debug/cache_fixture")], text=True)

        newer = jobs.fingerprint(second)
        state = worktrees.prepare_cargo_cache(second, target, newer)
        self.assertEqual(build(second), "second")
        self.assertEqual(jobs.fingerprint(second), newer)
        worktrees.finish_cargo_cache(state, jobs.fingerprint(second))
        for path in [first / "src/main.rs", first / "src/value.txt"]:
            os.utime(path, (1, 1))
        older = jobs.fingerprint(first)
        state = worktrees.prepare_cargo_cache(first, target, older)
        self.assertTrue((target / "debug/cache_fixture").exists(), "keep executable outputs")
        self.assertTrue(dependency.is_dir(), "keep registry dependencies")
        self.assertFalse(worktrees.cargo_workspace_fingerprints(target, {"cache_fixture"}))
        self.assertEqual(build(first), "first")
        # A stable run records current Cargo inputs and retains the next run's cache.
        self.assertEqual(jobs.fingerprint(first), older)
        worktrees.finish_cargo_cache(state, jobs.fingerprint(first))
        signature = worktrees.cargo_artifact_signature(target, {"cache_fixture"})
        self.assertTrue(signature)
        worktrees.prepare_cargo_cache(first, target, older)
        self.assertEqual(worktrees.cargo_artifact_signature(target, {"cache_fixture"}), signature)
        self.assertEqual((dependency / "lib-serde").read_text(), "preserve dependency freshness")

    def test_shared_cargo_rejects_an_unrecorded_writer_in_every_profile(self):
        self.init_repo()
        self.cargo_fixture(self.root, "current")
        target = self.root / "target"
        source = jobs.fingerprint(self.root)
        state = worktrees.prepare_cargo_cache(self.root, target, source)
        entries = []
        for profile in ["debug", "release", "aarch64-unknown-linux-gnu/release"]:
            entry = target / profile / ".fingerprint/cache_fixture-0000000000000000/lib-cache_fixture"
            entry.parent.mkdir(parents=True)
            entry.write_text("same fingerprint bytes")
            entries.append(entry)
        unrelated = target / "debug/.fingerprint/cache_fixture-extra-0000000000000000"
        unrelated.mkdir()
        worktrees.finish_cargo_cache(state, source)
        changed = entries[0]
        stamp = changed.stat().st_mtime_ns + 1_000_000_000
        os.utime(changed, ns=(stamp, stamp))
        worktrees.prepare_cargo_cache(self.root, target, source)
        self.assertTrue(all(not entry.exists() for entry in entries))
        self.assertTrue(unrelated.is_dir(), "package-name prefixes do not identify dependencies")

    def test_shared_cargo_never_certifies_inputs_changed_during_a_job(self):
        self.init_repo()
        self.cargo_fixture(self.root, "before")
        target = self.root / "target"
        source = jobs.fingerprint(self.root)
        state = worktrees.prepare_cargo_cache(self.root, target, source)
        entry = target / "debug/.fingerprint/cache_fixture-0000000000000000/lib-cache_fixture"
        entry.parent.mkdir(parents=True)
        entry.write_text("compiled before")
        (self.root / "src/value.txt").write_text("after")
        changed = jobs.fingerprint(self.root)
        self.assertNotEqual(changed, source)
        worktrees.finish_cargo_cache(state, changed)
        self.assertFalse((target / ".wrela-workspace.json").exists())
        worktrees.prepare_cargo_cache(self.root, target, changed)
        self.assertFalse(entry.exists())

    def test_subdirectory_admission_protects_the_whole_workspace_and_its_inputs(self):
        self.init_repo()
        self.cargo_fixture(self.root, "workspace")
        member = self.root / "member"
        member.mkdir()
        self.cargo_fixture(member, "member")
        (member / "Cargo.toml").write_text(
            '[package]\nname = "member_fixture"\nversion = "0.1.0"\nedition = "2024"\n')
        root_manifest = self.root / "Cargo.toml"
        root_manifest.write_text(root_manifest.read_text().replace(
            '[workspace]', '[workspace]\nmembers = ["member"]\nexclude = ["excluded"]'))
        source = jobs.fingerprint(member / "src")
        self.assertEqual(source, jobs.fingerprint(self.root))
        self.assertEqual(worktrees.cargo_workspace_packages(member / "src"),
                         ["cache_fixture", "member_fixture"])
        target = self.root / "target"
        entry = target / "release/.fingerprint/cache_fixture-0000000000000000/lib-cache_fixture"
        entry.parent.mkdir(parents=True)
        entry.write_text("another checkout's workspace dependency")
        state = worktrees.prepare_cargo_cache(member / "src", target, source)
        self.assertFalse(entry.exists())
        self.assertEqual(state["identity"]["checkout"], str(self.root))
        (self.root / "src/value.txt").write_text("untracked input outside the current subdirectory")
        self.assertNotEqual(jobs.fingerprint(member / "src"), source)
        # Independent fixtures must not acquire their containing project's package set.
        excluded = self.root / "excluded"
        excluded.mkdir()
        self.cargo_fixture(excluded, "excluded")
        manifest = excluded / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('[workspace]\n', ''))
        self.assertEqual(worktrees.cargo_workspace_root(excluded / "src"), excluded)
        self.assertEqual(worktrees.cargo_workspace_packages(excluded / "src"), ["cache_fixture"])

    def test_nested_admission_rejects_checkout_switches_without_requeueing(self):
        self.init_repo()
        other = worktrees.create(self.root, "other-lease")
        inside = self.root / "inside"
        inside.mkdir()
        script = f'''import json, os, pathlib, subprocess, sys
sys.path.insert(0, {str(TOOLS)!r})
import jobs
assert jobs.admitted()
os.chdir({str(inside)!r})
assert jobs.admitted(), "a subdirectory stays in the same checkout"
os.chdir({str(other)!r})
try:
    jobs.run("build", [sys.executable, "-c", "raise SystemExit(99)"])
except ValueError as error:
    assert "queue work in" in str(error)
    print(str(error))
else:
    raise AssertionError("cross-checkout admission was accepted")
os.chdir({str(self.root)!r})
assert jobs.admitted(), "rejecting another checkout must preserve the original lease"
'''
        code, output = self.result(self.queued(script))
        self.assertEqual(code, 0, output)
        self.assertEqual(output.count("queued "), 1, output)
        self.assertIn("queue work in", output)

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
            self.assertEqual(subprocess.check_output(["git", "branch", "--list", "codex/one"], cwd=self.root).strip(), b"+ codex/one")
            worktrees.remove(self.root, "two")
            self.assertFalse(second.exists())
            self.assertFalse(subprocess.check_output(["git", "branch", "--list", "codex/two"], cwd=self.root).strip())
            self.assertNotIn("two", worktrees._registry(self.root / ".git")[1])

    def test_squash_removal_deletes_branch_after_main_advances(self):
        self.init_repo()
        with patch.dict(os.environ, self.env):
            tree = worktrees.create(self.root, "squashed")
            (tree / "source").write_text("merged source")
            subprocess.run(["git", "add", "source"], cwd=tree, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "branch work"], cwd=tree, check=True)
            subprocess.run(["git", "merge", "--squash", "codex/squashed"], cwd=self.root, check=True, capture_output=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "squash"], cwd=self.root, check=True)
            (self.root / "source").write_text("later main work")
            subprocess.run(["git", "add", "source"], cwd=self.root, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "advance main"], cwd=self.root, check=True)
            self.assertEqual(subprocess.run(["git", "merge-base", "--is-ancestor", "codex/squashed", "main"], cwd=self.root).returncode, 1)
            worktrees.remove(self.root, "squashed")
            self.assertFalse(tree.exists())
            self.assertFalse(subprocess.check_output(["git", "branch", "--list", "codex/squashed"], cwd=self.root).strip())
            self.assertFalse(worktrees._registry(self.root / ".git")[1])
            self.assertEqual((self.root / "source").read_text(), "later main work")

    def test_squash_removal_preserves_followup_commits(self):
        self.init_repo()
        with patch.dict(os.environ, self.env):
            tree = worktrees.create(self.root, "followup")
            for source in ("merged source", "unmerged followup"):
                (tree / "source").write_text(source)
                subprocess.run(["git", "add", "source"], cwd=tree, check=True)
                subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", source], cwd=tree, check=True)
                if source == "merged source":
                    subprocess.run(["git", "merge", "--squash", "codex/followup"], cwd=self.root, check=True, capture_output=True)
                    subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "squash"], cwd=self.root, check=True)
            head = subprocess.check_output(["git", "rev-parse", "codex/followup"], cwd=self.root)
            with self.assertRaisesRegex(ValueError, "commits not in main"):
                worktrees.remove(self.root, "followup")
            self.assertTrue(tree.exists())
            self.assertEqual(subprocess.check_output(["git", "rev-parse", "codex/followup"], cwd=self.root), head)
            self.assertIn("followup", worktrees._registry(self.root / ".git")[1])

    def test_removal_preserves_worktree_on_another_branch(self):
        self.init_repo()
        with patch.dict(os.environ, self.env):
            tree = worktrees.create(self.root, "switched")
            subprocess.run(["git", "switch", "-qc", "other"], cwd=tree, check=True)
            (tree / "source").write_text("other branch work")
            subprocess.run(["git", "add", "source"], cwd=tree, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "other branch"], cwd=tree, check=True)
            with self.assertRaisesRegex(ValueError, "different branch"):
                worktrees.remove(self.root, "switched")
            self.assertTrue(tree.exists())
            self.assertEqual((tree / "source").read_text(), "other branch work")
            self.assertTrue(subprocess.check_output(["git", "branch", "--list", "codex/switched"], cwd=self.root).strip())

    def test_removal_preserves_commit_racing_branch_deletion(self):
        self.init_repo()
        with patch.dict(os.environ, self.env):
            tree = worktrees.create(self.root, "raced")
            head = subprocess.check_output(["git", "rev-parse", "codex/raced"], cwd=self.root).strip().decode()
            (self.root / "source").write_text("concurrent followup")
            subprocess.run(["git", "add", "source"], cwd=self.root, check=True)
            future_tree = subprocess.check_output(["git", "write-tree"], cwd=self.root).strip().decode()
            future = subprocess.check_output(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit-tree", future_tree, "-p", head], cwd=self.root, input=b"followup\n").strip().decode()
            run = subprocess.run

            def advance_after_removal(command, **kwargs):
                result = run(command, **kwargs)
                if command[:3] == ["git", "worktree", "remove"]:
                    run(["git", "update-ref", "refs/heads/codex/raced", future, head], cwd=self.root, check=True)
                return result

            with patch.object(worktrees.subprocess, "run", side_effect=advance_after_removal):
                with self.assertRaises(subprocess.CalledProcessError):
                    worktrees.remove(self.root, "raced")
            self.assertFalse(tree.exists())
            self.assertEqual(subprocess.check_output(["git", "rev-parse", "codex/raced"], cwd=self.root).strip().decode(), future)
            self.assertEqual(subprocess.check_output(["git", "show", "codex/raced:source"], cwd=self.root), b"concurrent followup")
            self.assertNotIn("raced", worktrees._registry(self.root / ".git")[1])

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
                self.assertTrue(tree.exists())
                self.assertTrue(subprocess.check_output(["git", "branch", "--list", "codex/busy"], cwd=self.root).strip())
                record.unlink()
            (tree / "source").write_text("new commit")
            subprocess.run(["git", "add", "source"], cwd=tree, check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "unmerged"], cwd=tree, check=True)
            with self.assertRaisesRegex(ValueError, "commits not in main"):
                worktrees.remove(self.root, "busy")
            self.assertTrue(tree.exists())
            self.assertEqual((tree / "source").read_text(), "new commit")

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

    def test_cache_scan_is_reused_until_pruning_and_budget_still_fails_closed(self):
        self.init_repo()
        cache = self.root / ".git/wrela/cache/cargo"
        cache.mkdir(parents=True)
        incremental = cache / "debug/incremental"
        for amounts, prune, fails in (([5], False, False), ([11], False, True),
                                      ([11, 5], True, False), ([11, 11], True, True)):
            with self.subTest(amounts=amounts, prune=prune):
                if prune:
                    incremental.mkdir(parents=True)
                    (incremental / "disposable").write_text("cache")
                with patch.dict(os.environ, self.env), \
                        patch.object(worktrees, "CACHE_BUDGET", 10), \
                        patch.object(worktrees, "allocated", side_effect=amounts) as scan, \
                        patch.object(worktrees.shutil, "disk_usage", return_value=type("Disk", (), {"free": 100 * worktrees.GIB})()):
                    if fails:
                        with self.assertRaisesRegex(ValueError, "shared caches exceed"):
                            worktrees.maintain(self.root)
                    else:
                        worktrees.maintain(self.root)
                    self.assertEqual(scan.call_count, len(amounts))
                self.assertFalse(incremental.exists())
        self.assertEqual((self.root / "source").read_text(), "source")

    def test_low_disk_pruning_rechecks_under_budget_cache_and_retains_disk_guard(self):
        self.init_repo()
        incremental = self.root / ".git/wrela/cache/cargo/debug/incremental"
        incremental.mkdir(parents=True)
        (incremental / "disposable").write_text("cache")
        with patch.dict(os.environ, self.env), \
                patch.object(worktrees, "MIN_FREE", 12 * worktrees.GIB), \
                patch.object(worktrees, "CACHE_BUDGET", 10), \
                patch.object(worktrees, "allocated", side_effect=[5, 1]) as scan, \
                patch.object(worktrees.shutil, "disk_usage", return_value=type("Disk", (), {"free": 2 * worktrees.GIB})()):
            with self.assertRaisesRegex(ValueError, "12 GiB"):
                worktrees.maintain(self.root)
            self.assertEqual(scan.call_count, 2)
        self.assertFalse(incremental.exists())
        self.assertEqual((self.root / "source").read_text(), "source")


if __name__ == "__main__":
    unittest.main()
