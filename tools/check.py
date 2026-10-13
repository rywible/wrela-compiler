"""The local check: the complete suite, or exactly one named check.

Every retained test runs, including GPU and browser tests. There are no tiers, remembered
passes or environment-selected sizes. Compiled artifacts are cached; test results are not.
"""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import xml.etree.ElementTree as ET

from chrome_session import ChromeSession
import runner

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ("examples", "engine", "compiler/std", "compiler/tests/fields", "compiler/tests/math",
           "compiler/tests/sketches", "compiler/tests/numerics", "compiler/tests/render",
           "compiler/tests/queries", "compiler/tests/simd", "compiler/tests/input",
           "compiler/tests/lift", "compiler/tests/texels", "compiler/tests/shader_params",
           "compiler/tests/unroll", "ui", "studio", "platform", "console")
STEPS = ("rustfmt", "clippy", "wrela-fmt", "tools", "browser-types", "browser-dist",
         "browser-size", "browser", "doc", "wgsl")
CONTROLS = ("WRELA_RUN_ONLY", "WRELA_BLESS", "WRELA_FUZZ_ITERS", "WRELA_FUZZ_SEED",
            "WRELA_FUZZ_WORKER", "WRELA_FUZZ_JOBS", "WRELA_FUZZ_TIERS", "WRELA_FULL",
            "WRELA_CONST_CACHE")


def selection(arguments):
    if len(arguments) > 1 or any(a.startswith("-") or not a.strip() for a in arguments):
        raise ValueError("usage: tools/check.sh [check-name] (one name, no flags)")
    return arguments[0] if arguments else None


def call(command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


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


def rust_packages(name):
    """Narrow executable lookup; the executable resolves the exact test name.

    Generated tests fall back to searching the workspace build.
    """
    leaf = name.rsplit("::", 1)[-1]
    packages = set()
    for top in ("compiler", "runtime"):
        for path in (ROOT / top).rglob("*.rs"):
            if "target" in path.relative_to(ROOT).parts:
                continue
            if not re.search(r"\bfn\s+" + re.escape(leaf) + r"\s*\(", path.read_text()):
                continue
            for parent in path.parents:
                manifest = parent / "Cargo.toml"
                if manifest.exists():
                    match = re.search(r'^name\s*=\s*"([^"]+)"', manifest.read_text(), re.M)
                    if match:
                        packages.add(match[1])
                    break
    return sorted(packages)


def build_tests(name=None):
    packages = rust_packages(name) if name else []
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
    if packages:
        programs = [p for p in programs if any(
            re.search(r'^name\s*=\s*"' + re.escape(pkg) + '"',
                      Path(p["manifest_path"]).read_text(), re.M) for pkg in packages)]
    return runner.in_order(programs)


def listed(program):
    result = call([program["executable"], "--include-ignored", "--list", "--format=terse"],
                  capture_output=True, text=True)
    return [line[:-6] for line in result.stdout.splitlines() if line.endswith(": test")]


def matching(programs, name):
    matches = []
    for program in programs:
        manifest = Path(program["manifest_path"]).read_text()
        package = re.search(r'^name\s*=\s*"([^"]+)"', manifest, re.M)[1]
        for test in listed(program):
            canonical = f"{package}::{program['target']['name']}::{test}"
            if name in (test, canonical):
                matches.append((program, test, canonical))
    if len(matches) != 1:
        choices = ", ".join(m[2] for m in matches)
        raise ValueError(f"check name must identify exactly one test; {len(matches)} matches" +
                         (f": {choices}" if choices else f" for {name!r}"))
    return matches[0][:2]


def rust_run(program, logs, name=None):
    cache = os.environ.get("WRELA_NATIVE_CACHE", str(Path(os.environ.get("CARGO_TARGET_DIR", "target")) / "native-cache"))
    command, env, cwd, scratch = runner.start(program, runner.scratch_root(), os.path.abspath(cache))
    args = ["--include-ignored"]
    if name:
        args += ["--exact", name, "--nocapture"]
    expected = 1 if name else len(listed({**program, "executable": str(Path(scratch) / "test-program")}))
    started = time.monotonic()
    print(f"== {name or runner.name_of(program)}", flush=True)
    output_path = Path(scratch) / "run.log"
    with output_path.open("w") as output_file:
        result = subprocess.run([*command, *args], cwd=cwd, env=env,
                                stdout=output_file, stderr=subprocess.STDOUT, text=True)
    output = output_path.read_text(errors="replace")
    count = sum(int(n) for n in re.findall(r"test result: \w+\. (\d+) passed", output))
    if result.returncode or count != expected or re.search(r"; [1-9]\d* ignored;", output):
        print(output, flush=True)
        raise ValueError(f"{runner.name_of(program)} failed or skipped tests; scratch: {scratch}")
    shutil.rmtree(scratch)
    if name:
        print(output, end="", flush=True)
    print(f"{count} passed in {time.monotonic() - started:.1f}s", flush=True)
    return count


def browser_test(name, logs):
    args = ["bun", "test", "--reporter=junit", f"--reporter-outfile={logs / 'browser.xml'}"]
    if name:
        parts = name.split("::", 2)
        if len(parts) != 3 or not parts[2]:
            raise ValueError("browser test name: browser::<file.test.ts>::<full test name>")
        path = ROOT / "runtime/browser/test" / parts[1]
        if path.parent != ROOT / "runtime/browser/test" or not path.is_file():
            raise ValueError("unknown browser test file")
        args += [str(path), "--test-name-pattern", "^" + re.escape(parts[2]) + "$"]
    call(args, cwd=ROOT / "runtime/browser")
    cases = list(ET.parse(logs / "browser.xml").iter("testcase"))
    ran = [case for case in cases if case.find("skipped") is None]
    if not ran or (name and len(ran) != 1) or (not name and len(ran) != len(cases)):
        raise ValueError(f"browser selection ran {len(ran)} tests; expected " +
                         ("exactly one" if name else "every test, without skips"))
    return len(ran)


def step(name, wrela, logs):
    commands = {
        "rustfmt": ["cargo", "fmt", "--all", "--", "--check"],
        "clippy": ["cargo", "clippy", "-q", "--workspace", "--all-targets", "--", "-D", "warnings"],
        "wrela-fmt": [str(wrela), "fmt", "--check", *SOURCES],
        "doc": ["cargo", "test", "-q", "--release", "--workspace", "--doc"],
        "wgsl": [sys.executable, "tools/wgsl_budgets.py", str(wrela)],
    }
    if name == "tools":
        if runner.tools_tests():
            raise ValueError("tools tests failed")
    elif name.startswith("tools::"):
        selected = name[len("tools::"):]
        if selected not in tools_ids():
            raise ValueError(f"unknown tools test {selected!r}")
        passed, output = runner._tool_test(selected, str(ROOT / "tools/tests"))
        print(output, end="", flush=True)
        if not passed:
            raise ValueError(f"{name} failed")
    elif name == "browser" or name.startswith("browser::"):
        browser_test(None if name == "browser" else name, logs)
    elif name.startswith("browser-"):
        script = {"browser-types": "typecheck", "browser-dist": "check-dist", "browser-size": "size"}[name]
        call(["bun", "run", script], cwd=ROOT / "runtime/browser")
    else:
        call(commands[name])


def wrela_test(name, executable, logs):
    parts = name.split("::")
    if len(parts) != 3 or not parts[1] or not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", parts[2]):
        raise ValueError("Wrela names are wrela::<package-path>::<test-name>")
    package = (ROOT / parts[1]).resolve()
    if not package.is_relative_to(ROOT) or not package.is_dir():
        raise ValueError("Wrela package must be a directory inside this checkout")
    options = []
    if package == ROOT / "compiler/std":
        package = logs / "std-program"
        package.mkdir()
        (package / "main.wrela").write_text("")
        options.append("--std")
    result = call([str(executable), "test", str(package), parts[2], "--exact", "--json", *options],
                  capture_output=True, text=True)
    report = json.loads(result.stdout)
    tests = report["tests"]
    if len(tests) != 1 or tests[0]["name"] != parts[2] or not tests[0]["passed"]:
        raise ValueError("the Wrela runner must execute exactly the selected test successfully")
    print(f"ok {name} ({tests[0]['at']})", flush=True)


def main(arguments):
    name = selection(arguments)
    os.chdir(ROOT)
    for key in CONTROLS:
        os.environ.pop(key, None)
    started = time.monotonic()
    wrela = Path(os.environ.get("CARGO_TARGET_DIR", "target")).resolve() / "release/wrela"
    with tempfile.TemporaryDirectory(prefix="wrela-check-") as temporary:
        logs = Path(temporary)
        if name in STEPS or (name and name.startswith(("tools::", "browser::"))):
            if name in ("wrela-fmt", "wgsl"):
                call(["cargo", "build", "-q", "--release", "-p", "wrela", "--bin", "wrela"])
            step(name, wrela, logs)
        elif name and name.startswith("wrela::"):
            build_tests()  # The same dependency features and executable as the full suite.
            # `cargo test --no-run` builds the CLI's test harness, not its normal executable.
            call(["cargo", "build", "-q", "--release", "-p", "wrela", "--bin", "wrela"])
            wrela_test(name, wrela, logs)
        else:
            programs = build_tests(name)
            if name:
                program, test = matching(programs, name)
                with ChromeSession(logs):
                    rust_run(program, logs, test)
            else:
                call(["cargo", "build", "-q", "--release", "-p", "wrela", "--bin", "wrela"])
                for check in STEPS:
                    print(f"== {check}", flush=True)
                    step_started = time.monotonic()
                    step(check, wrela, logs)
                    print(f"{check} passed in {time.monotonic() - step_started:.1f}s", flush=True)
                with ChromeSession(logs):
                    count = sum(rust_run(p, logs) for p in programs if listed(p))
                if not count:
                    raise ValueError("the suite discovered no Rust tests")
                print(f"{count} Rust tests passed", flush=True)
    print(f"{name or 'all checks'} passed in {time.monotonic() - started:.1f}s", flush=True)
    return 0


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
