"""Run test programs with isolated scratch and report every failure.

Each Rust executable is hard-linked into its own scratch directory, so a concurrent
build cannot replace the program while it is spawning workers. Test results are never cached.
"""

import errno
import os
import shutil
import tempfile


def _tool_test(test_id, directory):
    """Run exactly one unittest and return its result and captured output."""
    import contextlib
    import io
    import sys
    import unittest

    if directory not in sys.path:
        sys.path.insert(0, directory)
    output = io.StringIO()
    with contextlib.chdir(directory), contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
        suite = unittest.defaultTestLoader.loadTestsFromName(test_id)
        result = unittest.TextTestRunner(stream=output, verbosity=0).run(suite)
    return result.wasSuccessful() and result.testsRun == 1 and not result.skipped, output.getvalue()


def tools_tests(directory="tools/tests"):
    """Run every tools test, preserving failures and rejecting skips or empty discovery."""
    import unittest

    directory = os.path.abspath(directory)
    suite = unittest.TestLoader().discover(directory)
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    return 0 if result.wasSuccessful() and result.testsRun and not result.skipped else 1


def scratch_root():
    """Where the programs' scratch directories are: beneath the checkout even when Cargo's target
    is elsewhere (browser pages are served from it), at target/tmp's depth (copied fixtures have
    relative dependency paths)."""
    root = os.path.abspath("target")
    os.makedirs(root, exist_ok=True)
    return root


def is_first(program):
    """Whether `program` (cargo's artifact message) is the end-to-end suite's."""
    return os.path.dirname(program["manifest_path"]).endswith("compiler/tests")


def in_order(programs):
    """The programs, the end-to-end suite first."""
    return sorted(programs, key=lambda m: not is_first(m))


def start(program, root, native_cache, threads):
    """(the command's start, its environment, its directory, its scratch) for `program`, which
    runs `threads` tests at a time: append its arguments to the command."""
    pkg = os.path.dirname(program["manifest_path"])
    scratch = tempfile.mkdtemp(prefix="test-scratch-", dir=root)
    # A concurrent Cargo build may replace its test binary. Keep this run's executable reachable
    # so tests that spawn themselves (the fuzz workers) can still find it.
    executable = os.path.join(scratch, "test-program")
    try:
        os.link(program["executable"], executable)
    except OSError as error:
        if error.errno != errno.EXDEV:
            raise
        shutil.copy2(program["executable"], executable)
    env = {**os.environ, "CARGO_MANIFEST_DIR": pkg, "WRELA_NATIVE_CACHE": native_cache,
           "WRELA_TEST_TMPDIR": scratch}
    return [executable, f"--test-threads={threads}"], env, pkg, scratch


def name_of(program):
    return f"{program['target']['name']} ({program['target']['kind'][0]})"


if __name__ == "__main__":
    import sys
    if sys.argv[1:] != ["--tools-tests"]:
        sys.exit("usage: tools/runner.py --tools-tests")
    sys.exit(tools_tests())
