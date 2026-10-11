"""Run test programs with isolated scratch, bounded threads and shared compiled artifacts.

Each Rust executable is hard-linked into its own scratch directory, so another checkout's
build cannot replace the program while it is spawning workers. Test results are never cached.
"""

import errno
import os
import shutil
import tempfile


def _tool_test(test_id, directory):
    """Run one unittest in a reusable worker, keeping each test's own fixtures and output."""
    import contextlib
    import io
    import sys
    import unittest

    os.chdir(directory)
    if directory not in sys.path:
        sys.path.insert(0, directory)
    output = io.StringIO()
    with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
        suite = unittest.defaultTestLoader.loadTestsFromName(test_id)
        result = unittest.TextTestRunner(stream=output, verbosity=0).run(suite)
    return result.wasSuccessful() and result.testsRun == 1 and not result.skipped, output.getvalue()


def tools_tests(directory="tools/tests"):
    """Run the tools' tests with bounded processes and import each module once per worker.

    Each test still runs its setup and cleanup, and different tests never share a worker at the
    same time. Reusing workers avoids paying Python startup per test.
    """
    from concurrent.futures import ProcessPoolExecutor
    import unittest
    from resources import worker_count

    directory = os.path.abspath(directory)
    ids = []

    def walk(suite):
        for test in suite:
            if isinstance(test, unittest.TestSuite):
                walk(test)
            else:
                ids.append(test.id())

    walk(unittest.TestLoader().discover(directory))
    failed = 0
    with ProcessPoolExecutor(max_workers=min(2, worker_count())) as pool:
        results = pool.map(_tool_test, ids, [directory] * len(ids))
        for passed, output in results:
            if not passed:
                failed += 1
                print(output)
    print(f"{len(ids)} tools tests, {failed} failed")
    return 1 if failed or not ids else 0


def scratch_root():
    """Where the programs' scratch directories are: beneath the checkout even when Cargo's target
    is elsewhere (browser pages are served from it), at target/tmp's depth (copied fixtures have
    relative dependency paths). Old ones are removed."""
    root = os.path.abspath("target")
    os.makedirs(root, exist_ok=True)
    from worktrees import prune_scratch
    prune_scratch(root)
    return root


def is_first(program):
    """Whether `program` (cargo's artifact message) is the end-to-end suite's."""
    return os.path.dirname(program["manifest_path"]).endswith("compiler/tests")


def in_order(programs):
    """The programs, the end-to-end suite first."""
    return sorted(programs, key=lambda m: not is_first(m))


def gpu_shared(cores):
    """How many of a program's GPU tests share the GPU at a time."""
    return 2


def start(program, root, logs, native_cache, cores):
    """(the command's start, its environment, its directory, its scratch) for `program`: append
    its arguments to the command. Two test threads, bounded by the admitted CPU allowance,
    on a software GPU too."""
    pkg = os.path.dirname(program["manifest_path"])
    scratch = tempfile.mkdtemp(prefix="test-scratch-", dir=root)
    with open(os.path.join(scratch, ".owner"), "w") as owner:
        owner.write(str(os.getpid()))
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
           "WRELA_TEST_TMPDIR": scratch,
           "WRELA_GPU_SHARED": str(gpu_shared(cores)), "WRELA_GPU_WAITS": f"{logs}/gpu-waits"}
    command = [] if is_first(program) else ["nice", "-n", "10"]
    return [*command, executable, f"--test-threads={max(1, min(2, cores))}"], env, pkg, scratch


def name_of(program):
    return f"{program['target']['name']} ({program['target']['kind'][0]})"


if __name__ == "__main__":
    import sys
    if sys.argv[1:] != ["--tools-tests"]:
        sys.exit("usage: tools/runner.py --tools-tests")
    sys.exit(tools_tests())
