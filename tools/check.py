"""The local check: the complete suite, exactly one named check, or one group of tests.

Every retained test runs, including GPU and browser tests. There are no tiers, remembered
passes or environment-selected sizes. Compiled artifacts are cached; test results are not.

The suite runs Clippy and the doc tests, then two lanes at once: the other steps, in turn, and
the Rust test programs, in turn, each running several tests at a time. Every check runs even
when another fails, and each failure's output is shown at the end.
"""

from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import xml.etree.ElementTree as ET

from chrome_session import ChromeSession
import runner

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ("examples", "games", "engine", "compiler/std", "compiler/tests/fields",
           "compiler/tests/math", "compiler/tests/sketches", "compiler/tests/numerics",
           "compiler/tests/render", "compiler/tests/queries", "compiler/tests/simd",
           "compiler/tests/input", "compiler/tests/lift", "compiler/tests/texels",
           "compiler/tests/shader_params", "compiler/tests/unroll", "ui", "studio",
           "studio/tests/views", "docsite", "platform",
           "console")
# Clippy and the doc tests hold Cargo's lock and many cores, so they run before the lanes.
CARGO_STEPS = ("clippy", "doc")
LIGHT_STEPS = ("rustfmt", "wrela-fmt", "tools", "browser-types", "browser-dist", "browser-size",
               "browser", "wgsl")
STEPS = CARGO_STEPS + LIGHT_STEPS
CONTROLS = ("WRELA_RUN_ONLY", "WRELA_BLESS", "WRELA_FUZZ_WORKER", "WRELA_CONST_CACHE")
PRINT = threading.Lock()


def selection(arguments):
    if len(arguments) > 1 or any(a.startswith("-") or not a.strip() for a in arguments):
        raise ValueError("usage: tools/check.sh [check-name] (one name, no flags)")
    return arguments[0] if arguments else None


def threads():
    """How many tests a program runs at a time: half the cores, from 2 to 6. Tests start their
    own worker pools, so more threads contend for the cores rather than finish sooner."""
    return max(2, min(6, (os.cpu_count() or 2) // 2))


def call(command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


def into(out):
    """Arguments that send a command's output to `out`, or leave it on the terminal."""
    return {} if out is None else {"stdout": out, "stderr": subprocess.STDOUT}


def tools_ids():
    ids = []
    def walk(suite):
        for test in suite:
            if isinstance(test, unittest.TestSuite):
                walk(test)
            else:
                ids.append(test.id())
    walk(unittest.TestLoader().discover(str(ROOT / "tools/tests")))
    return ids


def build_tests():
    # Keep workspace feature unification identical for a single test and the suite. A -p
    # build changes dependency features and can trigger another expensive compilation.
    result = call(["cargo", "test", "-q", "--release", "--workspace", "--no-run", "--message-format=json"],
                  capture_output=True, text=True)
    programs = []
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable") \
                and message["profile"]["test"]:
            programs.append(message)
    return runner.in_order(programs)


def listed(program):
    result = call([program["executable"], "--include-ignored", "--list", "--format=terse"],
                  capture_output=True, text=True)
    return [line[:-6] for line in result.stdout.splitlines() if line.endswith(": test")]


def program_name(program):
    """`package::target`, the start of each of the program's canonical test names."""
    manifest = Path(program["manifest_path"]).read_text()
    return re.search(r'^name\s*=\s*"([^"]+)"', manifest, re.M)[1] + "::" + program["target"]["name"]


def matching(programs, name):
    matches = []
    for program in programs:
        start = program_name(program)
        for test in listed(program):
            canonical = f"{start}::{test}"
            if name in (test, canonical):
                matches.append((program, test, canonical))
    if len(matches) != 1:
        choices = ", ".join(m[2] for m in matches)
        raise ValueError(f"check name must identify exactly one test; {len(matches)} matches" +
                         (f": {choices}" if choices else f" for {name!r}"))
    return matches[0][:2]


def group(programs, prefix):
    """The one program with tests whose path starts with `prefix` (`module::` or
    `package::target::module::`), and those tests. A path containing it further in is not one."""
    found = []
    for program in programs:
        start = program_name(program)
        tests = [test for test in listed(program)
                 if test.startswith(prefix) or f"{start}::{test}".startswith(prefix)]
        if tests:
            found.append((program, tests))
    if len(found) != 1:
        names = ", ".join(program_name(program) for program, _ in found)
        raise ValueError(f"group {prefix!r} must select the tests of exactly one program; " +
                         (f"{len(found)} have them: {names}" if found else "none has any"))
    return found[0]


def rust_run(program, out=None, names=None):
    """Runs every test of `program`, or exactly `names`: how many passed. The output goes to
    `out` (standard output by default) when the run fails or when one test was named."""
    out = out or sys.stdout
    cache = os.environ.get("WRELA_NATIVE_CACHE", str(Path(os.environ.get("CARGO_TARGET_DIR", "target")) / "native-cache"))
    command, env, cwd, scratch = runner.start(program, runner.scratch_root(), os.path.abspath(cache), threads())
    args = ["--include-ignored"]
    if names:
        args += ["--exact", *names]
        if len(names) == 1:
            args.append("--nocapture")
    expected = len(names) if names else len(listed({**program, "executable": str(Path(scratch) / "test-program")}))
    output_path = Path(scratch) / "run.log"
    with output_path.open("w") as output_file:
        result = subprocess.run([*command, *args], cwd=cwd, env=env,
                                stdout=output_file, stderr=subprocess.STDOUT, text=True)
    output = output_path.read_text(errors="replace")
    count = sum(int(n) for n in re.findall(r"test result: \w+\. (\d+) passed", output))
    if result.returncode or count != expected or re.search(r"; [1-9]\d* ignored;", output):
        out.write(output)
        out.flush()
        raise ValueError(f"{runner.name_of(program)} failed or skipped tests; scratch: {scratch}")
    shutil.rmtree(scratch)
    if names and len(names) == 1:
        out.write(output)
    return count


def browser_test(name, logs, out=None):
    """Every browser test, the tests of one file (`browser::<file>::`) or one test."""
    args = ["bun", "test", "--reporter=junit", f"--reporter-outfile={logs / 'browser.xml'}"]
    exact = False
    if name:
        parts = name.split("::", 2)
        if len(parts) != 3:
            raise ValueError("browser test names: browser::<file.test.ts>::<full test name>, "
                             "or browser::<file.test.ts>:: for every test in the file")
        path = ROOT / "runtime/browser/test" / parts[1]
        if path.parent != ROOT / "runtime/browser/test" or not path.is_file():
            raise ValueError("unknown browser test file")
        args.append(str(path))
        if parts[2]:
            exact = True
            args += ["--test-name-pattern", "^" + re.escape(parts[2]) + "$"]
    call(args, cwd=ROOT / "runtime/browser", **into(out))
    cases = list(ET.parse(logs / "browser.xml").iter("testcase"))
    ran = [case for case in cases if case.find("skipped") is None]
    if not ran or (exact and len(ran) != 1) or (not exact and len(ran) != len(cases)):
        raise ValueError(f"browser selection ran {len(ran)} tests; expected " +
                         ("exactly one" if exact else "every test, without skips"))
    return len(ran)


def browser_packages(out=None):
    """The browser runtime's locked packages (TypeScript, the WebGPU types): a fresh checkout has none."""
    call(["bun", "install", "--frozen-lockfile", "--silent"], cwd=ROOT / "runtime/browser", **into(out))


def step(name, wrela, logs, out=None):
    """Runs one infrastructure check, its output going to `out` (by default the terminal)."""
    commands = {
        "rustfmt": ["cargo", "fmt", "--all", "--", "--check"],
        "clippy": ["cargo", "clippy", "-q", "--workspace", "--all-targets", "--", "-D", "warnings"],
        "wrela-fmt": [str(wrela), "fmt", "--check", *SOURCES],
        "doc": ["cargo", "test", "-q", "--release", "--workspace", "--doc"],
        "wgsl": [sys.executable, "tools/wgsl_budgets.py", str(wrela)],
    }
    if name == "tools":
        # In a process of its own: the tools' tests patch modules, the environment and the
        # working directory, and their fake browsers must not reach the check's Chrome.
        environment = {k: v for k, v in os.environ.items() if k != "WRELA_TEST_CHROME"}
        call([sys.executable, "tools/runner.py", "--tools-tests"], cwd=ROOT, env=environment, **into(out))
    elif name.startswith("tools::"):
        selected = name[len("tools::"):]
        if selected not in tools_ids():
            raise ValueError(f"unknown tools test {selected!r}")
        passed, output = runner._tool_test(selected, str(ROOT / "tools/tests"))
        print(output, end="", flush=True)
        if not passed:
            raise ValueError(f"{name} failed")
    elif name == "browser" or name.startswith("browser::"):
        browser_packages(out)
        browser_test(None if name == "browser" else name, logs, out)
    elif name.startswith("browser-"):
        script = {"browser-types": "typecheck", "browser-dist": "check-dist", "browser-size": "size"}[name]
        browser_packages(out)
        call(["bun", "run", script], cwd=ROOT / "runtime/browser", **into(out))
    else:
        call(commands[name], **into(out))


def wrela_test(name, executable, logs):
    """One Wrela test (`wrela::<package>::<test>`) or all of a package's (`wrela::<package>::`)."""
    parts = name.split("::")
    if len(parts) != 3 or not parts[1] or not re.fullmatch(r"([A-Za-z_][A-Za-z_0-9]*)?", parts[2]):
        raise ValueError("Wrela names are wrela::<package-path>::<test-name>, "
                         "or wrela::<package-path>:: for every test in the package")
    package = (ROOT / parts[1]).resolve()
    if not package.is_relative_to(ROOT) or not package.is_dir():
        raise ValueError("Wrela package must be a directory inside this checkout")
    options = []
    if package == ROOT / "compiler/std":
        package = logs / "std-program"
        package.mkdir()
        (package / "main.wrela").write_text("")
        options.append("--std")
    selected = [parts[2], "--exact"] if parts[2] else []
    result = call([str(executable), "test", str(package), *selected, "--json", *options],
                  capture_output=True, text=True)
    tests = json.loads(result.stdout)["tests"]
    if parts[2] and (len(tests) != 1 or tests[0]["name"] != parts[2]):
        raise ValueError("the Wrela runner must execute exactly the selected test")
    failed = [test["name"] for test in tests if not test["passed"]]
    if not tests or failed:
        raise ValueError(f"the Wrela runner ran {len(tests)} tests" +
                         (f"; failed: {', '.join(failed)}" if failed else ""))
    for test in tests:
        print(f"ok wrela::{parts[1]}::{test['name']} ({test['at']})", flush=True)


def job(name, logs, work):
    """Runs `work(output)`, writing its output to a log of its own. Any failure is recorded,
    not raised, so the other checks still run: (name, the failure or None, the log)."""
    log = logs / "jobs" / (re.sub(r"[^\w.-]+", "-", name) + ".log")
    log.parent.mkdir(exist_ok=True)
    started = time.monotonic()
    with log.open("w") as output:
        try:
            work(output)
            failure = None
        except subprocess.CalledProcessError as error:
            output.write(error.stdout or "")
            output.write(error.stderr or "")
            failure = f"{error.cmd[0]} exited with status {error.returncode}"
        except Exception as error:
            failure = f"{type(error).__name__}: {error}"
    with PRINT:
        print(f"{'ok' if failure is None else 'FAILED'} {name} in {time.monotonic() - started:.1f}s", flush=True)
    return name, failure, log


def suite(wrela, logs):
    """Every check: 0 when all pass, else 1 after showing each failure."""
    programs = [program for program in build_tests() if listed(program)]
    call(["cargo", "build", "-q", "--release", "-p", "wrela", "--bin", "wrela"])
    results = [job(check, logs, lambda out, check=check: step(check, wrela, logs, out))
               for check in CARGO_STEPS]
    counts = []
    print(f"Rust test programs run {threads()} tests at a time", flush=True)
    with ChromeSession(logs), ThreadPoolExecutor(1) as lane:
        light = lane.submit(lambda: [
            job(check, logs, lambda out, check=check: step(check, wrela, logs, out))
            for check in LIGHT_STEPS])
        for program in programs:
            results.append(job(program_name(program), logs,
                               lambda out, program=program: counts.append(rust_run(program, out))))
        results += light.result()
    failures = [(name, failure, log) for name, failure, log in results if failure]
    for name, failure, log in failures:
        print(f"\n== {name}: {failure}\n{log.read_text(errors='replace')}", end="", flush=True)
    if not failures and not sum(counts):
        raise ValueError("the suite discovered no Rust tests")
    print(f"\n{sum(counts)} Rust tests passed" if not failures else
          f"\n{len(failures)} of {len(results)} checks failed: {', '.join(f[0] for f in failures)}", flush=True)
    return 1 if failures else 0


def main(arguments):
    name = selection(arguments)
    os.chdir(ROOT)
    for key in CONTROLS:
        os.environ.pop(key, None)
    started = time.monotonic()
    wrela = Path(os.environ.get("CARGO_TARGET_DIR", "target")).resolve() / "release/wrela"
    status = 0
    with tempfile.TemporaryDirectory(prefix="wrela-check-") as temporary:
        logs = Path(temporary)
        if name is None:
            status = suite(wrela, logs)
        elif name in STEPS or name.startswith(("tools::", "browser::")):
            if name in ("wrela-fmt", "wgsl"):
                call(["cargo", "build", "-q", "--release", "-p", "wrela", "--bin", "wrela"])
            step(name, wrela, logs)
        elif name.startswith("wrela::"):
            build_tests()  # The same dependency features and executable as the full suite.
            # `cargo test --no-run` builds the CLI's test harness, not its normal executable.
            call(["cargo", "build", "-q", "--release", "-p", "wrela", "--bin", "wrela"])
            wrela_test(name, wrela, logs)
        else:
            programs = build_tests()
            if name.endswith("::"):
                program, tests = group(programs, name)
            else:
                program, test = matching(programs, name)
                tests = [test]
            with ChromeSession(logs):
                count = rust_run(program, names=tests)
            print(f"{count} passed", flush=True)
    print(f"{name or 'all checks'} {'passed' if status == 0 else 'failed'} in {time.monotonic() - started:.1f}s",
          flush=True)
    return status


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except subprocess.CalledProcessError as error:
        if error.stdout:
            print(error.stdout)
        if error.stderr:
            print(error.stderr, file=sys.stderr)
        sys.exit(error.returncode)
    except (OSError, ValueError, KeyError) as error:
        sys.exit(f"check: {error}")
