"""How the gate runs one of its test programs: tools/check.sh's runner and tools/memo.py's quick
runs both start each program this way, so the two can't drift.

A program runs as cargo runs it (in its package's directory), from a link to its executable in a
scratch directory of its own (`target/test-scratch-*`, its tests' WRELA_TEST_TMPDIR), with up to
four test threads, its GPU tests sharing the GPU, and its compiled code kept across runs. The
end-to-end suite (compiler/tests) is the longest program, so the gate's time: it starts first, and
the others yield to it (`nice`: with all at one priority, it took half as long again as alone)."""

import errno
import os
import shutil
import tempfile
import time

# A failed program's scratch is kept to look at for this long.
KEEP_SCRATCH = 6 * 3600


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
    return result.wasSuccessful(), output.getvalue()


def tools_tests(directory="tools/tests"):
    """Run the tools' tests with bounded processes and import each module once per worker.

    Each test still runs its setup and cleanup, and different tests never share a worker at the
    same time. Reusing workers avoids paying Python startup and the memo's AST scan per test.
    """
    from concurrent.futures import ProcessPoolExecutor
    import unittest
    from resources import cpu_count

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
    with ProcessPoolExecutor(max_workers=min(8, cpu_count() * 2)) as pool:
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
    for name in os.listdir(root):
        path = os.path.join(root, name)
        try:
            if name.startswith("test-scratch-") and time.time() - os.stat(path).st_mtime > KEEP_SCRATCH:
                shutil.rmtree(path, ignore_errors=True)
        except OSError:
            pass
    return root


def is_first(program):
    """Whether `program` (cargo's artifact message) is the end-to-end suite's."""
    return os.path.dirname(program["manifest_path"]).endswith("compiler/tests")


def in_order(programs):
    """The programs, the end-to-end suite first."""
    return sorted(programs, key=lambda m: not is_first(m))


def gpu_shared(cores):
    """How many of a program's GPU tests share the GPU at a time."""
    return max(2, min(4, cores))


def start(program, root, logs, native_cache, cores):
    """(the command's start, its environment, its directory, its scratch) for `program`: append
    its arguments to the command. Up to four test threads, bounded by the CPUs and the shared GPU
    limit, on a software GPU too: one thread a program made the memo-off gate's tests take 460 s,
    not 197 s (4 cores)."""
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
           "WRELA_TEST_TMPDIR": scratch, "RUST_TEST_THREADS": str(max(1, min(4, cores))),
           "WRELA_GPU_SHARED": str(gpu_shared(cores)), "WRELA_GPU_WAITS": f"{logs}/gpu-waits"}
    command = [] if is_first(program) else ["nice", "-n", "10"]
    return [*command, executable], env, pkg, scratch


def name_of(program):
    return f"{program['target']['name']} ({program['target']['kind'][0]})"


if __name__ == "__main__":
    import sys
    if sys.argv[1:] != ["--tools-tests"]:
        sys.exit("usage: tools/runner.py --tools-tests")
    sys.exit(tools_tests())
