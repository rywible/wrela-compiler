"""Tests for tools/memo.py, the gate's memory of slow tests that passed: what it reads from a
trace, what an entry keeps and matches, which sources a test's code can come from, and its
timings.

    python3 -m unittest discover -s tools/tests
"""

import os
import json
import shutil
import subprocess
import sys
import tempfile
import unittest

TESTS = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.path.dirname(TESTS)
sys.path.insert(0, TOOLS)

import memo  # noqa: E402

# A trace as `strace -f -y -e trace=%file` writes it, of two processes.
TRACE = r"""101 execve("/usr/bin/prog", ["prog", "--flag", "/not/a/path"], 0x7ffd /* 3 vars */) = 0
101 openat(AT_FDCWD</work>, "/etc/ld.so.cache", O_RDONLY|O_CLOEXEC) = 3</etc/ld.so.cache>
101 openat(AT_FDCWD</work>, "engine/a.wrela", O_RDONLY|O_CLOEXEC) = 4</work/engine/a.wrela>
101 openat(AT_FDCWD</work>, "missing.toml", O_RDONLY|O_CLOEXEC) = -1 ENOENT (No such file or directory)
101 openat(5</work/engine>, "b.wrela", O_RDONLY|O_CLOEXEC) = 6</work/engine/b.wrela>
101 openat(AT_FDCWD</work>, "/work/engine", O_RDONLY|O_NONBLOCK|O_CLOEXEC|O_DIRECTORY) = 7</work/engine>
102 openat(AT_FDCWD</work>, "/work/out/x.png", O_WRONLY|O_CREAT|O_TRUNC|O_CLOEXEC <unfinished ...>
101 newfstatat(AT_FDCWD</work>, "/work/wrela.toml", {st_mode=S_IFREG|0644, st_size=10, ...}, 0) = 0
102 <... openat resumed>) = 8</work/out/x.png>
101 statx(AT_FDCWD</work>, "/work/out/x.png", AT_STATX_SYNC_AS_STAT, STATX_ALL, {...}) = 0
101 openat(AT_FDCWD</work>, "/work/sp\x61ce d.txt", O_RDONLY) = 9</work/space d.txt>
101 rename("/work/tmp1", "/work/kept") = 0
101 +++ exited with 0 +++
"""


class Touched(unittest.TestCase):
    def test_a_trace_gives_each_path_and_how_it_was_touched(self):
        seen = memo.touched(TRACE)
        self.assertEqual(seen["/usr/bin/prog"], "read")
        self.assertNotIn("/not/a/path", seen)  # an argument of the program, not a file it opened
        self.assertEqual(seen["/work/engine/a.wrela"], "read")  # relative to the directory
        self.assertEqual(seen["/work/missing.toml"], "read")  # absent: a file the test looked for
        self.assertEqual(seen["/work/engine/b.wrela"], "read")  # relative to a directory's fd
        self.assertEqual(seen["/work/engine"], "list")
        self.assertEqual(seen["/work/wrela.toml"], "stat")
        self.assertEqual(seen["/work/space d.txt"], "read")  # an escaped character

    def test_a_path_written_is_never_an_input(self):
        seen = memo.touched(TRACE)
        # Written by another process in a call strace split in two, then asked about.
        self.assertEqual(seen["/work/out/x.png"], "write")
        self.assertEqual(seen["/work/tmp1"], "write")
        self.assertEqual(seen["/work/kept"], "write")


class Files(unittest.TestCase):
    def setUp(self):
        self.root = os.path.realpath(tempfile.mkdtemp())

    def tearDown(self):
        shutil.rmtree(self.root)

    def write(self, rel, text):
        path = os.path.join(self.root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
        return path


class Inputs(Files):
    def test_outputs_and_the_machines_own_state_are_left_out(self):
        target = os.path.join(self.root, "target")
        inputs = memo.Inputs(self.root, target)
        a = self.write("engine/a.wrela", "a")
        seen = {a: "read", os.path.join(target, "release/x"): "read", "/proc/self/maps": "read",
                os.path.join(self.root, "ex/build/consts"): "list",
                os.path.join(self.root, "ex/build/consts/ab12.wcv"): "read",
                os.path.join(self.root, "out.png"): "write", os.path.join(self.root, "gone"): "read"}
        got = inputs.of(seen, memo.Digests())
        self.assertEqual(set(got), {"repo:engine/a.wrela", "repo:gone"})
        self.assertTrue(got["repo:engine/a.wrela"].startswith("file:"))
        self.assertEqual(got["repo:gone"], "absent")
        self.assertEqual(inputs.path("repo:engine/a.wrela"), a)

    def test_a_listed_directorys_state_is_its_names(self):
        d = memo.Digests()
        self.write("pkg/a.wrela", "a")
        before = d.compute(os.path.join(self.root, "pkg"), "list")
        self.write("pkg/b.wrela", "b")
        self.assertNotEqual(d.compute(os.path.join(self.root, "pkg"), "list"), before)
        self.assertEqual(d.compute(os.path.join(self.root, "pkg"), "stat"), "dir")


class Store(Files):
    def test_an_entry_matches_only_with_the_same_key_environment_and_inputs(self):
        store = memo.Store(self.root)
        store.save("p:suite:test:a::b", "rust1", {"WRELA_FULL": None}, {"repo:x": "file:1"}, 7.0)
        same = {"repo:x": "file:1"}

        def find(rust="rust1", env=None, now=same):
            env = env or {}
            return store.find("p:suite:test:a::b", rust, lambda k: env.get(k),
                              lambda name, state: now.get(name) == state)

        self.assertIsNotNone(find())
        self.assertIsNone(find(rust="rust2"))  # the Rust sources changed
        self.assertIsNone(find(env={"WRELA_FULL": "1"}))  # a variable the code reads
        self.assertIsNone(find(now={"repo:x": "file:2"}))  # a file it read
        self.assertIsNone(store.find("p:suite:test:a::c", "rust1", lambda k: None, lambda n, s: True))

    def test_a_time_beside_the_other_tests_counts_only_until_the_test_runs_alone(self):
        store = memo.Store(self.root)
        self.assertIsNone(store.timing("t"))
        store.set_timing("t", 8.0, alone=False)
        self.assertEqual(store.timing("t"), 8.0)
        store.set_timing("t", 0.3, alone=True)
        store.set_timing("t", 300.0, alone=False)  # waiting for the GPU beside the others
        self.assertEqual(store.timing("t"), 0.3)
        # A time alone from long ago no longer counts.
        path = os.path.join(store.dir, "timings", memo.sha("t")[:24] + ".json")
        with open(path) as f:
            t = memo.json.load(f)
        t["alone_when"] -= (memo.KEEP_DAYS + 1) * 86400
        with open(path, "w") as f:
            memo.json.dump(t, f)
        self.assertEqual(store.timing("t"), 300.0)

    def test_old_entries_are_pruned(self):
        store = memo.Store(self.root)
        store.save("t", "rust", {}, {}, 1.0)
        d = store.entries_dir("t", "rust")
        (name,) = os.listdir(d)
        old = os.path.getmtime(os.path.join(d, name)) - (memo.KEEP_DAYS + 1) * 86400
        os.utime(os.path.join(d, name), (old, old))
        store.prune()
        self.assertFalse(os.path.exists(d))


class Sources(Files):
    PACKAGES = {
        "lib": {"name": "lib", "deps": [], "crates": ["lib"]},
        "comp": {"name": "wrela-sema", "deps": ["lib"], "crates": ["wrela_sema"]},
        "t": {"name": "t", "deps": ["lib", "comp"], "crates": ["t", "suite"]},
        "other": {"name": "other", "deps": [], "crates": ["other"]},
    }

    def setUp(self):
        super().setUp()
        subprocess.run(["git", "init", "-q", self.root], check=True)
        self.write("Cargo.toml", "[workspace]\n")
        self.write("lib/src/lib.rs", 'pub fn f() { std::env::var("WRELA_FULL"); }\n')
        self.write("comp/src/lib.rs", 'const STD: &str = include_str!("../../spec/std.txt");\n')
        self.write("spec/std.txt", "std\n")
        self.write("other/src/lib.rs", "pub fn g() {}\n")
        self.write("t/tests/suite/main.rs", "mod a;\nmod b;\nmod c;\nfn shared() { c::help() }\n")
        self.write("t/tests/suite/a.rs", "// b:: in a comment names nothing\nfn x() { super::shared() }\n")
        self.write("t/tests/suite/b.rs", "fn y() { wrela_sema::check(); wrela_syntax::is_name(); }\n")
        self.write("t/tests/suite/c.rs", "pub fn help() {}\n")
        self.write("target/release/deps/suite-1.d",
                   "/x/suite-1.d: t/tests/suite/main.rs t/tests/suite/a.rs lib/src/lib.rs\n")
        self.write("target/release/deps/wrela_sema-2.d", "/x/wrela_sema-2.d: comp/src/lib.rs spec/std.txt\n")

    def sources(self):
        return memo.Sources(self.root, os.path.join(self.root, "target"), self.PACKAGES)

    def test_a_test_leaves_out_the_modules_nothing_it_runs_names(self):
        s = self.sources()
        files = s.for_test(s.files_of(s.closure("t")), "t/tests/suite", "a::x")
        self.assertIn("t/tests/suite/a.rs", files)
        self.assertIn("t/tests/suite/c.rs", files)  # the program's root names it
        self.assertNotIn("t/tests/suite/b.rs", files)
        self.assertIn("lib/src/lib.rs", files)
        # A change to b leaves a's key as it was; one to the library doesn't.
        key = s.key(files)
        self.write("t/tests/suite/b.rs", "fn y() { changed() }\n")
        self.assertEqual(self.sources().key(files), key)
        self.write("lib/src/lib.rs", "pub fn f() {}\n")
        self.assertNotEqual(self.sources().key(files), key)

    def test_a_test_in_the_root_keeps_what_the_root_names_and_an_unknown_module_all(self):
        s = self.sources()
        files = s.files_of(s.closure("t"))
        top = s.for_test(files, "t/tests/suite", "top_level")
        self.assertIn("t/tests/suite/c.rs", top)
        self.assertNotIn("t/tests/suite/b.rs", top)
        self.assertIn("t/tests/suite/b.rs", s.for_test(files, "t/tests/suite", "elsewhere::x"))
        self.assertEqual(s.for_test(files, None, "a::x"), files)

    def test_a_crates_sources_are_its_own_and_those_it_is_built_with(self):
        s = self.sources()
        self.assertEqual(s.closure("t"), {"t", "lib", "comp"})
        files = s.files_of(s.closure("t"))
        self.assertIn("Cargo.toml", files)
        self.assertIn("spec/std.txt", files)  # the compiler's, by its dep-info
        self.assertNotIn("other/src/lib.rs", files)  # a crate it isn't built with
        self.assertEqual(s.closure("not-a-crate"), set(self.PACKAGES))
        self.assertTrue(s.compile_only("comp"))
        self.assertFalse(s.compile_only("t"))

    def test_the_variables_the_code_reads_are_found(self):
        s = self.sources()
        self.assertEqual(memo.env_names([s.text("lib/src/lib.rs")]), {"WRELA_FULL"})
        self.assertEqual(memo.env_names(['os.environ.get("WRELA_CHROME")']), {"WRELA_CHROME"})


# A test's trace with a build in it: the build reads the engine and the package; the test, after,
# its build's files (in target/) and a fixture.
BUILT = r"""300 execve("/w/target/release/deps/suite-9", ["suite-9"], 0x7ffd /* 3 vars */) = 0
300 statx(AT_FDCWD</w/t>, "/wrela-gate-memo/build-begin", AT_STATX_SYNC_AS_STAT, STATX_ALL, 0x7f) = -1 ENOENT (No such file or directory)
301 openat(AT_FDCWD</w/t>, "/w/engine", O_RDONLY|O_NONBLOCK|O_CLOEXEC|O_DIRECTORY) = 3</w/engine>
301 openat(AT_FDCWD</w/t>, "/w/engine/sky.wrela", O_RDONLY|O_CLOEXEC) = 4</w/engine/sky.wrela>
301 openat(AT_FDCWD</w/t>, "/w/ex/wrela.toml", O_RDONLY|O_CLOEXEC) = 5</w/ex/wrela.toml>
300 statx(AT_FDCWD</w/t>, "/wrela-gate-memo/build-end", AT_STATX_SYNC_AS_STAT, STATX_ALL, 0x7f) = -1 ENOENT (No such file or directory)
300 openat(AT_FDCWD</w/t>, "/w/target/tmp/b/game.wasm", O_RDONLY|O_CLOEXEC) = 6</w/target/tmp/b/game.wasm>
300 openat(AT_FDCWD</w/t>, "/w/t/fixture.json", O_RDONLY|O_CLOEXEC) = 7</w/t/fixture.json>
"""


class Trace(unittest.TestCase):
    def test_what_only_a_build_read_is_told_apart(self):
        t = memo.Trace(BUILT)
        self.assertEqual(t.ran, ["/w/target/release/deps/suite-9"])
        self.assertEqual(t.in_builds_only, {"/w/engine", "/w/engine/sky.wrela", "/w/ex/wrela.toml"})
        self.assertEqual(t.seen["/w/engine"], "list")
        self.assertNotIn(memo.BUILD_BEGIN, t.seen.keys() - {memo.BUILD_BEGIN})

    def test_a_path_also_read_outside_a_build_is_the_tests(self):
        t = memo.Trace(BUILT + '300 openat(AT_FDCWD</w/t>, "/w/engine/sky.wrela", O_RDONLY) = 8</w/engine/sky.wrela>\n')
        self.assertNotIn("/w/engine/sky.wrela", t.in_builds_only)


class Builds(Sources):
    """An entry keyed on its builds' files: which runs may have one, and when it matches."""

    def setUp(self):
        super().setUp()
        self.write("engine/sky.wrela", "fn sky() {}\n")
        self.write("ex/wrela.toml", "[package]\n")
        self.write("t/fixture.json", "{}\n")
        self.memo = memo.Memo(self.root, os.path.join(self.root, "target"), os.path.join(self.root, "store"),
                              sources=self.sources())
        self.trace = BUILT.replace("/w/", self.root + "/")
        self.record = json.dumps({"pkg": self.root + "/ex", "args": ["--testing"], "files": "h1"}) + "\n"

    def remember(self, trace=None, record=None, test="a::x"):
        return self.memo.remember("t:suite:test:" + test, "t/tests/suite", test,
                                  trace or self.trace, 1.0, self.record if record is None else record)

    def remembered(self, test="a::x"):
        self.memo.digests = memo.Digests()
        return self.memo.remembered("t:suite:test:" + test, "t/tests/suite", test)

    def test_a_run_whose_compiler_work_was_all_in_its_builds_is_keyed_on_their_files(self):
        self.assertEqual(self.remember(), "built")
        self.memo.rebuilds.known[(self.root + "/ex", ("--testing",))] = "h1"
        self.assertTrue(self.remembered())
        # The compiler changed, and the engine, but the build made the same files.
        self.write("comp/src/lib.rs", "// changed\n")
        self.write("engine/sky.wrela", "fn sky() { changed() }\n")
        self.memo.sources = self.sources()
        self.assertTrue(self.remembered())
        # The build's files differ.
        self.memo.rebuilds.known[(self.root + "/ex", ("--testing",))] = "h2"
        self.assertFalse(self.remembered())

    def test_the_tests_own_inputs_and_the_crates_that_run_still_count(self):
        self.remember()
        self.memo.rebuilds.known[(self.root + "/ex", ("--testing",))] = "h1"
        self.write("t/fixture.json", "{1}\n")
        self.assertFalse(self.remembered())
        self.write("t/fixture.json", "{}\n")
        self.write("lib/src/lib.rs", "// changed\n")
        self.memo.sources = self.sources()
        self.assertFalse(self.remembered())

    def test_compiler_work_outside_a_build_keys_the_run_on_the_compilers_sources(self):
        outside = self.record + '{"outside": true}\n'
        self.assertEqual(self.remember(record=outside), "crates")
        read = self.trace + f'300 openat(AT_FDCWD</w/t>, "{self.root}/engine/sky.wrela", O_RDONLY) = 8</x>\n'
        self.assertEqual(self.remember(trace=read), "crates")
        self.assertEqual(self.remember(test="b::y"), "crates")  # its code names the compiler
        no_command = json.dumps({"pkg": self.root + "/ex", "args": None, "files": "h"}) + "\n"
        self.assertEqual(self.remember(record=no_command), "crates")
        written = json.dumps({"pkg": self.root + "/target/tmp/p", "args": [], "files": "h"}) + "\n"
        self.assertEqual(self.remember(record=written), "crates")
        ran = self.trace + f'300 execve("{self.root}/target/release/wrela", ["wrela"], 0x7f) = 0\n'
        self.assertEqual(self.remember(trace=ran), "all")

    def test_the_files_hash_is_compiler_tests(self):
        # The same as `files_hash_is_tools_memos` in compiler/tests/src/lib.rs.
        d = os.path.join(self.root, "build")
        os.makedirs(os.path.join(d, "sub"))
        with open(os.path.join(d, ".wrela-build"), "w") as f:
            f.write("sub/b.wgsl\na.wasm\n")
        with open(os.path.join(d, "a.wasm"), "wb") as f:
            f.write(b"\0asm")
        with open(os.path.join(d, "sub/b.wgsl"), "wb") as f:
            f.write(b"fn main() {}")
        self.assertEqual(memo.Rebuilds.files_hash(d), FILES_HASH)


FILES_HASH = "8410ef328ec8af3db236a70a705c1c9c7746bdc9695323b3e5bb56ef647fdf28"


class Ending(unittest.TestCase):
    def test_stopping_kills_every_process_under_the_runner_and_the_ones_that_escaped(self):
        # A process orphaned under the runner (as a test is when strace, its parent, dies first)
        # comes back to it as its subreaper.
        script = f"""
import os, subprocess, sys, time
sys.path.insert(0, {TOOLS!r})
import memo
memo.subreaper()
sh = subprocess.Popen(["sh", "-c", "sleep 60 & sleep 60"])
time.sleep(0.5)
sh.kill()
sh.wait()
escaped = len(memo.under(os.getpid()))
memo.stop_all()
print(escaped, len(memo.under(os.getpid())))
"""
        out = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True,
                             timeout=30, check=True).stdout
        self.assertEqual(out.split(), ["2", "0"])


if __name__ == "__main__":
    unittest.main()
