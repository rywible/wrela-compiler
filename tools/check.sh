#!/usr/bin/env bash
# The local checks. There's no CI (by design), and nothing here is to be waited on idly: the gate
# takes seconds, and the longer tiers run in the background while the work goes on.
#
#   tools/check.sh          the gate: what must pass before a commit lands on main. Formatting,
#                           lints, and every test but the long ones and the measurements, on the
#                           CPU and the GPU (release builds): unit, conformance, diagnostics
#                           goldens, run and GPU tests, grammar and fuzz samples; the tools'
#                           tests; the browser runtime's checks; the WGSL size budgets. Its tests
#                           have a budget (below); it fails if they take longer.
#   tools/check.sh --long   also the long checks, at their smaller sizes: the tests whose
#                           `#[ignore]` reason starts `long:` (headless Chrome against the native
#                           host, the clearing's camera path, soak runs, images against spike
#                           01's), sharing the GPU; the grammar's 10^6 programs and 200,000 GBNF
#                           samples; 20,000 fuzz cases from a new seed. About ten minutes: before
#                           a branch is merged, in the background.
#   tools/check.sh --full   also everything at full size, and the measurements: the long checks
#                           with `WRELA_FULL`, every test at full size, the tests whose reason
#                           starts `measure:` (time budgets and comparisons with spike 01) one at
#                           a time with the GPU alone, the CLI's speed budgets, and 10^6 fuzz
#                           cases. Hours: on an idle machine, when a milestone's numbers are
#                           recorded.
#   tools/check.sh <filter>...
#                           the edit loop's check: only the gate's tests whose names hold a
#                           filter (`clearing::`, `lock`), from every test program, the GPU
#                           shared; with --long, the long checks' too. Seconds.
#
# Needs: the Rust toolchain in rust-toolchain.toml, bun, python3, a GPU; Chrome stable for --long.
set -euo pipefail
cd "$(dirname "$0")/.."

# The gate runs everything, the same way each time: a variable that narrows the tests, rewrites
# goldens or picks a fuzz seed (left set in the shell by an earlier run) doesn't apply here.
unset WRELA_RUN_ONLY WRELA_BLESS WRELA_FUZZ_ITERS WRELA_FUZZ_SEED WRELA_FUZZ_WORKER WRELA_FUZZ_JOBS \
  WRELA_FUZZ_TIERS WRELA_FULL WRELA_GPU_SHARED

long=0 full=0 filters=()
for arg in "$@"; do
  case "$arg" in
    --long) long=1 ;;
    --full) long=1 full=1 ;;
    -*) echo "usage: tools/check.sh [--long | --full] | [--long] <filter>..." >&2; exit 2 ;;
    *) filters+=("$arg") ;;
  esac
done
if [ "$full" = 1 ] && [ "${#filters[@]}" -gt 0 ]; then
  echo "a measurement runs alone: cargo test --release --workspace -- --ignored --exact <name>" >&2
  exit 2
fi

step() { printf '\n== %s\n' "$*"; }

# The long tests and the measurements, by name (`module::test`, or `test` in a test program of
# one file): each test whose `#[ignore]` reason starts `long:` is in `long_tests`, and `measure:`
# in `measures`.
long_tests=() measures=()
while read -r kind name; do
  if [ "$kind" = measure ]; then measures+=("$name"); else long_tests+=("$name"); fi
done < <(python3 - <<'PY'
import pathlib, re
for f in sorted([*pathlib.Path("compiler").rglob("*.rs"), *pathlib.Path("runtime").rglob("*.rs")]):
    if "target" in f.parts:
        continue
    lines = f.read_text().splitlines()
    for i, line in enumerate(lines):
        if m := re.search(r'#\[ignore = "(long|measure):', line):
            name = next(re.search(r"fn (\w+)\(", l).group(1) for l in lines[i + 1:] if re.search(r"fn (\w+)\(", l))
            # A test file at the top of `tests/` is a test program of its own (no module).
            print(m[1], name if f.parent.name == "tests" else f"{f.stem}::{name}")
PY
)

# wrela sources the formatter owns. The conformance and diagnostics cases aren't here: some
# are malformed on purpose, and their annotations are part of their layout.
WRELA_SOURCES=(examples engine compiler/std compiler/tests/fields compiler/tests/math compiler/tests/sketches
  compiler/tests/numerics compiler/tests/render compiler/tests/queries compiler/tests/simd
  compiler/tests/input compiler/tests/lift compiler/tests/texels compiler/tests/shader_params compiler/tests/unroll ui studio)

logs=$(mktemp -d)
side_jobs=()
side() { # name, command...: runs in the background, its output and status kept
  local name=$1; shift
  side_jobs+=("$name")
  (set +e; "$@" > "$logs/$name" 2>&1; echo $? > "$logs/$name.status") &
}
# Waits for the side jobs: each that failed shows its output and fails the checks (`tests`).
side_results() {
  wait
  for job in "${side_jobs[@]}"; do
    if [ "$(cat "$logs/$job.status")" != 0 ]; then cat "$logs/$job"; echo "the $job checks failed" >&2; tests=1; fi
  done
}
# The tools' tests (the dev server, the headless-Chrome driver with a fake Chrome, the agent
# test's scorer) mostly wait on processes, so each runs in a process of its own, all at once.
# Each takes a lock file of its own (its TMPDIR), so they queue neither on the GPU nor together.
tools_tests() {
  python3 - <<'TOOLS'
import concurrent.futures, subprocess, sys, unittest
ids = []
def walk(suite):
    for t in suite:
        walk(t) if isinstance(t, unittest.TestSuite) else ids.append(t.id())
walk(unittest.defaultTestLoader.discover("tools/tests"))
def run(i):
    return subprocess.run([sys.executable, "-m", "unittest", "-q", i], cwd="tools/tests",
                          capture_output=True, text=True)
with concurrent.futures.ThreadPoolExecutor(len(ids)) as pool:
    failed = [p for p in pool.map(run, ids) if p.returncode != 0]
for p in failed:
    print(p.stderr)
print(f"{len(ids)} tools tests, {len(failed)} failed")
sys.exit(1 if failed else 0)
TOOLS
}

# Runs every test program at once, each with `args` (its tests filtered and skipped), in its
# package's directory as cargo does; prints each one's time, and the output of each that fails,
# and fails if no test ran. Four of a program's GPU tests share the GPU at a time: they check
# what the GPU computes, not how long it takes (the measurements run alone, below).
run_tests() { # args...
  python3 - "$logs" "${CARGO_TARGET_DIR:-target}/native-cache" "$@" <<'PY'
import json, os, re, subprocess, sys, time
logs, native_cache, *args = sys.argv[1:]
native_cache = os.path.abspath(native_cache)
runs = []
for line in open(f"{logs}/tests.json"):
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        # As cargo runs a test program: in its package's directory.
        pkg = os.path.dirname(m["manifest_path"])
        # Compiled code is kept across runs (wrela_host's cache): a test's build is made afresh
        # each run, and its WASM is mostly the same.
        env = {**os.environ, "CARGO_MANIFEST_DIR": pkg, "WRELA_NATIVE_CACHE": native_cache,
               "WRELA_GPU_SHARED": "4"}
        out = open(f"{logs}/run-{len(runs)}", "w+")
        name = f"{m['target']['name']} ({m['target']['kind'][0]})"
        proc = subprocess.Popen([m["executable"], "-q", *args], cwd=pkg, env=env,
                                stdout=out, stderr=subprocess.STDOUT)
        runs.append((name, proc, out, time.monotonic()))
failed, ran = False, 0
waiting = list(runs)
while waiting:
    time.sleep(0.02)
    for run in [r for r in waiting if r[1].poll() is not None]:
        waiting.remove(run)
        name, proc, out, start = run
        out.seek(0)
        text = out.read()
        passed = sum(int(n) for n in re.findall(r"test result: \w+\. (\d+) passed", text))
        ran += passed
        if proc.returncode != 0:
            failed = True
            print(text)
            print(f"{name} failed")
        elif passed and time.monotonic() - start >= 1:
            print(f"  {name}: {time.monotonic() - start:.1f}s")
print(f"  {ran} tests passed")
sys.exit(1 if failed or ran == 0 else 0)
PY
}

# Every test but the long ones and the measurements.
skips=()
for t in "${long_tests[@]}" "${measures[@]}"; do skips+=(--skip "$t"); done

# With filters, only the tests whose names hold one of them, of the gate's (and with --long,
# the long checks'): the edit loop's check, in seconds. Nothing else runs.
if [ "${#filters[@]}" -gt 0 ]; then
  step "build the tests (release)"
  cargo test -q --release --workspace --no-run --message-format=json > "$logs/tests.json"
  if [ "$long" = 1 ]; then
    skips=()
    for t in "${measures[@]}"; do skips+=(--skip "$t"); done
  fi
  step "the tests whose names hold ${filters[*]}"
  run_tests --include-ignored "${skips[@]}" "${filters[@]}"
  rm -rf "$logs"
  exit 0
fi

step "build the tests (release); beside it, rustfmt, clippy (warnings are errors), the tools' tests and the browser runtime's (tests, types, dist is current, size budget)"
started=$SECONDS
side rustfmt cargo fmt --all -- --check
side clippy cargo clippy -q --workspace --all-targets -- -D warnings
side tools tools_tests
side bun sh -c 'cd runtime/browser && bun run checks'
# The CLI is built with the tests, which run it. (A `cargo build -p wrela` beside this build
# would build the compiler's crates a second time: without the tests' dev-dependencies, some
# of their dependencies' features differ.)
cargo test -q --release --workspace --no-run --message-format=json > "$logs/tests.json"
wrela="${CARGO_TARGET_DIR:-target}/release/wrela"

step "wrela fmt --check"
"$wrela" fmt --check "${WRELA_SOURCES[@]}"

step "tests, on the CPU and the GPU, but the long ones and the measurements, every test program at once; beside them, the doc tests and the WGSL size budgets (every example's, sketch's and lens's pipelines)"
tests_started=$SECONDS
side doc cargo test -q --release --workspace --doc
side wgsl python3 tools/wgsl_budgets.py "$wrela"
tests=0
run_tests --include-ignored "${skips[@]}" || tests=1
side_results
[ "$tests" = 0 ] || exit 1
grep -h "pipelines in" "$logs/wgsl" || true
took=$((SECONDS - tests_started))
echo "  the tests took ${took}s; built and tested in $((SECONDS - started))s"
# The budget that keeps the gate quick: a test that takes seconds belongs in the long checks.
if [ "$took" -gt 60 ]; then
  echo "the tests took ${took}s, over their 60 s budget: tag the slow ones \`#[ignore = \"long: ...\"]\`" >&2
  exit 1
fi

if [ "$long" = 1 ]; then
  # A new seed each run, so the runs between them cover more cases; a failure names its seed.
  seed=$(( (RANDOM << 15 | RANDOM) + 1 ))
  side_jobs=()
  step "long: the long checks$( [ "$full" = 1 ] && echo ", at full size"), sharing the GPU (one headless Chrome at a time); beside them, the grammar's 10^6 generated programs against the oracle and the formatter, and 200,000 GBNF samples (seed $seed)"
  side differential cargo run -q --release -p wrela-grammar --bin differential -- 1000000 --seed "$seed"
  side gbnf cargo run -q --release -p wrela-grammar --bin gbnf-sample -- 200000 --seed "$seed"
  # More threads than cores: a test waiting for its turn at Chrome (one at a time) holds one.
  # With --full, at full size.
  if [ "$full" = 1 ]; then export WRELA_FULL=1; fi
  run_tests --ignored --test-threads 16 --exact "${long_tests[@]}" || tests=1
  unset WRELA_FULL
  side_results
  [ "$tests" = 0 ] || exit 1

  fuzz=$( [ "$full" = 1 ] && echo 1000000 || echo 20000 )
  step "long: fuzz: $fuzz mutated programs of tiers 1 and 2, one worker per core (seed $seed)"
  WRELA_FUZZ_ITERS=$fuzz WRELA_FUZZ_TIERS=12 WRELA_FUZZ_SEED=$seed \
    cargo test -q --release -p wrela-tests --test suite fuzz::
fi

if [ "$full" = 1 ]; then
  step "full: every test of the gate at full size"
  WRELA_FULL=1 run_tests --include-ignored "${skips[@]}"

  step "full: the CLI's speed, cold processes: hello field's check < 200 ms and build < 2 s, sketch 03's, the herd's, the clearing's and the lens's (on each subject) < 500 ms and < 5 s"
  python3 - "$wrela" <<'PY'
import shutil, subprocess, sys, tempfile, time
sys.path.insert(0, "tools")
from subjects import SUBJECTS, lens, package
wrela = sys.argv[1]
def best(args, n=5, cold=None):
    times = []
    for _ in range(n):
        # A build that keeps a cache (the lens's compiled code) is timed cold.
        if cold:
            shutil.rmtree(cold, ignore_errors=True)
        t = time.perf_counter()
        subprocess.run([wrela, *args], check=True, capture_output=True)
        times.append(time.perf_counter() - t)
    return min(times), max(times)
out = tempfile.mkdtemp()
try:
    hello, sketch = "examples/hello-field", "compiler/tests/sketches/03-simulation"
    # The lens on each subject (tools/subjects.py): `wrela studio` builds it lifted, then keeps
    # its compiled code.
    for s in SUBJECTS:
        subprocess.run([wrela, "studio", f"examples/{s}", "build"], check=True, capture_output=True)
    runs = [
        ("check hello field", ["check", hello], 0.2),
        ("build hello field", ["build", hello, "-o", f"{out}/speed-hello"], 2.0),
        ("check sketch 03", ["check", sketch], 0.5),
        ("build sketch 03", ["build", sketch, "-o", f"{out}/speed-sketch"], 5.0),
        ("check the herd", ["check", "examples/herd"], 0.5),
        ("build the herd", ["build", "examples/herd", "-o", f"{out}/speed-herd"], 5.0),
        ("check the clearing", ["check", "examples/clearing"], 0.5),
        ("build the clearing", ["build", "examples/clearing", "-o", f"{out}/speed-clearing"], 5.0),
    ]
    for s in SUBJECTS:
        runs += [
            (f"check the lens on the {s}", ["check", lens(s)], 0.5),
            (f"build the lens on the {s}", ["build", lens(s), "--lift", package(s), "-o", f"{out}/speed-lens-{s}"], 5.0),
            (f"studio build on the {s} (and its compiled code)", ["studio", f"examples/{s}", "build"], 5.0,
             f"examples/{s}/build/studio/page/.native"),
        ]
    for name, args, budget, *cold in runs:
        lo, hi = best(args, cold=cold[0] if cold else None)
        print(f"  wrela {name}: {lo * 1000:.0f}-{hi * 1000:.0f} ms (budget {budget * 1000:.0f} ms)")
        if hi > budget:
            sys.exit(f"wrela {name} took {hi * 1000:.0f} ms, over its budget")
finally:
    shutil.rmtree(out, ignore_errors=True)
PY

  # The measurements run one at a time, the GPU theirs alone, on the builds' own compiled code
  # (a load's time counts compiling it): the load-time ones measure shader compilation on the
  # CPU (with the other tests compiling beside them, spike 13's cold load took 2.6 s instead of
  # 0.8 s), and Chrome's input latency and text budgets measure real time (under the other
  # tests' load, an event missed its next frame, and 10,000 glyphs took 0.52 ms instead of
  # 0.23). So do the lens's: its frames' and clicks' times in Chrome, its drags in a session, its
  # fits' 10 s, and an edit's 2 s to show. So do the herd's ratios to spike 01 (#42): each
  # compares two timings taken side by side, and another test's load skews one of them.
  step "full: the measurements, one at a time"
  cargo test -q --release --workspace -- --ignored --exact --test-threads=1 "${measures[@]}"
fi
rm -rf "$logs"

printf '\nall checks passed\n'
