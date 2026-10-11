#!/usr/bin/env bash
# The local checks. There's no CI (by design). Filtered and warm checks are the edit loop;
# cold software-Vulkan checks and the longer tiers can take minutes.
#
#   tools/check.sh          the gate: what must pass before a commit lands on main. Formatting,
#                           lints, and every test but the long ones and the measurements, on the
#                           CPU and the GPU (release builds): unit, conformance, diagnostics
#                           goldens, run and GPU tests, grammar and fuzz samples; the tools'
#                           tests; the browser runtime's checks; the WGSL size budgets. Its tests
#                           have a budget (below); it fails if they take longer.
#   tools/check.sh --fresh  the gate, with no test skipped as remembered. The optional memory
#                           of slow tests requires Linux strace and mount namespaces.
#                           On a software GPU (WRELA_SOFTWARE_GPU, a cloud machine), the tests
#                           whose `#[ignore]` reason starts `heavy:` are long tests instead.
#   tools/check.sh --long   also the long checks, at their smaller sizes: the tests whose
#                           `#[ignore]` reason starts `long:` (headless Chrome against the native
#                           host, the clearing's camera path, soak runs, images against spike
#                           01's), sharing the GPU; the grammar's 10^6 programs and 200,000 GBNF
#                           samples; the WGSL size budgets of the packages that bake a
#                           history (the floor); 20,000 fuzz cases from a new seed. Before
#                           a branch is merged, in the background. Cold CPU runs take longer.
#   tools/check.sh --full   also everything at full size, and the measurements: the long checks
#                           with `WRELA_FULL`, every test at full size, the tests whose reason
#                           starts `measure:` (time budgets and comparisons with spike 01) one at
#                           a time with the GPU alone, the CLI's speed budgets, and 10^6 fuzz
#                           cases. Hours: on an idle machine, when a milestone's numbers are
#                           recorded.
#   tools/check.sh <filter>...
#                           the edit loop's check: only the gate's tests whose names hold a
#                           filter (`clearing::`, `lock`), from every test program, the GPU
#                           shared; with --long, the long checks' too; with --nocapture, each
#                           program's output, passed or not. Seconds.
#   tools/check.sh --package=<crate> [filter]...
#                           a focused check that builds only the selected crate's tests and
#                           dependencies; repeat --package to select more crates. --long and
#                           --nocapture apply as above. This does not replace the merge gate.
#   tools/check.sh --fmt    formats what the gate checks: rustfmt, and `wrela fmt` with the CLI
#                           as last built (tools/hooks.py runs it before the gate).
#
# Needs: the Rust toolchain in rust-toolchain.toml, bun, python3, a GPU; Chrome stable for --long.
# A cloud machine can use software Vulkan (tools/cloud.sh); set WRELA_SOFTWARE_GPU=1 so the
# hardware timing budget is reported there. Command/state checks use CPU hosts where possible;
# pixel/buffer comparisons still execute their real shaders.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$(python3 tools/resources.py)}"

# The gate runs everything, the same way each time: a variable that narrows the tests, rewrites
# goldens or picks a fuzz seed (left set in the shell by an earlier run) doesn't apply here.
unset WRELA_RUN_ONLY WRELA_BLESS WRELA_FUZZ_ITERS WRELA_FUZZ_SEED WRELA_FUZZ_WORKER WRELA_FUZZ_JOBS \
  WRELA_FUZZ_TIERS WRELA_FULL WRELA_GPU_SHARED WRELA_GPU_WAITS
# The tests count the constants their builds compute: each package keeps its own cache here, not
# the one a session shares with other checkouts.
unset WRELA_CONST_CACHE

usage="usage: tools/check.sh [--fresh] [--long] | --full | [--long] [--nocapture] [--package=<crate>]... [<filter>...] | --fmt"
long=0 full=0 fmt=0 fresh=0 nocapture=() filters=() packages=()
for arg in "$@"; do
  case "$arg" in
    --long) long=1 ;;
    --full) long=1 full=1 ;;
    --fmt) fmt=1 ;;
    --fresh) fresh=1 ;;
    --nocapture) nocapture=(--nocapture) ;;
    --package=) echo "$usage" >&2; exit 2 ;;
    --package=*) packages+=("${arg#--package=}") ;;
    -*) echo "$usage" >&2; exit 2 ;;
    *) filters+=("$arg") ;;
  esac
done
if [ "$full" = 1 ] && [ "${#filters[@]}" -gt 0 ]; then
  echo "a measurement runs alone: cargo test --release --workspace -- --ignored --exact <name>" >&2
  exit 2
fi
if { [ "${#packages[@]}" -gt 0 ] && { [ "$full" = 1 ] || [ "$fresh" = 1 ] || [ "$fmt" = 1 ]; }; } \
  || { [ "${#nocapture[@]}" -gt 0 ] && [ "${#filters[@]}" = 0 ] && [ "${#packages[@]}" = 0 ]; } \
  || { [ "$fmt" = 1 ] && [ "$#" -gt 1 ]; } \
  || { [ "$fresh" = 1 ] && { [ "$full" = 1 ] || [ "${#filters[@]}" -gt 0 ] || [ "${#nocapture[@]}" -gt 0 ]; }; }; then
  echo "$usage" >&2
  exit 2
fi

step() { printf '\n== %s\n' "$*"; }

# wrela sources the formatter owns. The conformance and diagnostics cases aren't here: some
# are malformed on purpose, and their annotations are part of their layout.
WRELA_SOURCES=(examples engine compiler/std compiler/tests/fields compiler/tests/math compiler/tests/sketches
  compiler/tests/numerics compiler/tests/render compiler/tests/queries compiler/tests/simd
  compiler/tests/input compiler/tests/lift compiler/tests/texels compiler/tests/shader_params compiler/tests/unroll ui studio)

# Formats what the gate checks, and names what it changed. `wrela fmt` is the CLI as last built:
# the gate builds it afresh and checks again.
if [ "$fmt" = 1 ]; then
  cargo fmt --all -- -l
  wrela="${CARGO_TARGET_DIR:-target}/release/wrela"
  if [ -x "$wrela" ]; then
    # The files it would change, on stdout (its count is on stderr).
    "$wrela" fmt --check "${WRELA_SOURCES[@]}" 2>/dev/null || true
    "$wrela" fmt "${WRELA_SOURCES[@]}"
  fi
  exit 0
fi

# Formatting does not need to scan the ignored tests. Their names are `module::test`, or
# `test` in a test program of one file: `long:` tests are in `long_tests`, `measure:` in
# `measures`, and `heavy:` in `heavy_tests`.
long_tests=() measures=() heavy_tests=()
while read -r kind name; do
  case $kind in
    measure) measures+=("$name") ;;
    heavy) heavy_tests+=("$name") ;;
    *) long_tests+=("$name") ;;
  esac
done < <(python3 - <<'PY'
import pathlib, re
for f in sorted([*pathlib.Path("compiler").rglob("*.rs"), *pathlib.Path("runtime").rglob("*.rs")]):
    if "target" in f.parts:
        continue
    lines = f.read_text().splitlines()
    next_functions = [None] * len(lines)
    name = None
    for i in range(len(lines) - 1, -1, -1):
        next_functions[i] = name
        if m := re.search(r"fn (\w+)\(", lines[i]):
            name = m.group(1)
    inside = False
    for i, line in enumerate(lines):
        if m := re.search(r'#\[ignore = "(long|measure|heavy):', line):
            name = next_functions[i]
            # A test file at the top of `tests/` is a test program of its own (no module). A unit
            # test in `src/` is in its file's module, under the directories it's in, and in its
            # `mod tests` (a crate's root file names no module, nor does a `mod.rs`).
            if f.parent.name == "tests":
                print(m[1], name)
            elif "src" in f.parts:
                path = [*f.parts[f.parts.index("src") + 1 : -1], f.stem]
                module = "".join(f"{p}::" for p in path if p not in ("lib", "main", "mod"))
                print(m[1], f"{module}{'tests::' if inside else ''}{name}")
            else:
                print(m[1], f"{f.stem}::{name}")
        inside |= bool(re.match(r"\s*mod tests\b", line))
PY
)
# A heavy test takes seconds on a GPU but minutes on a software one (lavapipe): there, it is a
# long test, which main's long run checks every six hours.
if [ -n "${WRELA_SOFTWARE_GPU:-}" ]; then long_tests+=("${heavy_tests[@]}"); fi

logs=$(mktemp -d)
side_jobs=()
side() { # name, command...: runs in the background, its output, status and time kept
  local name=$1; shift
  side_jobs+=("$name")
  (set +e; s=$SECONDS; "$@" > "$logs/$name" 2>&1; echo $? > "$logs/$name.status"; echo $((SECONDS - s)) > "$logs/$name.took") &
}
# Waits for the side jobs: each that failed shows its output and fails the checks (`tests`);
# each that took 10 s or more says how long.
side_results() {
  wait
  for job in "${side_jobs[@]}"; do
    if [ "$(cat "$logs/$job.status")" != 0 ]; then cat "$logs/$job"; echo "the $job checks failed" >&2; tests=1; fi
    if [ "$(cat "$logs/$job.took")" -ge 10 ]; then echo "  beside them, $job: $(cat "$logs/$job.took")s"; fi
  done
  side_jobs=()
}
# The tools' tests (the dev server, the headless-Chrome driver with a fake Chrome, the agent
# test's scorer) mostly wait on processes, so each runs in a process of its own, with at most
# two per available CPU (and eight in total).
# Each takes a lock file of its own (its TMPDIR), so they queue neither on the GPU nor together.
tools_tests() {
  python3 - <<'TOOLS'
import concurrent.futures, subprocess, sys, unittest
sys.path.insert(0, "tools")
from resources import cpu_count
ids = []
def walk(suite):
    for t in suite:
        walk(t) if isinstance(t, unittest.TestSuite) else ids.append(t.id())
walk(unittest.defaultTestLoader.discover("tools/tests"))
def run(i):
    return subprocess.run([sys.executable, "-m", "unittest", "-q", i], cwd="tools/tests",
                          capture_output=True, text=True)
with concurrent.futures.ThreadPoolExecutor(min(8, max(1, cpu_count() * 2))) as pool:
    failed = [p for p in pool.map(run, ids) if p.returncode != 0]
for p in failed:
    print(p.stderr)
print(f"{len(ids)} tools tests, {len(failed)} failed")
sys.exit(1 if failed else 0)
TOOLS
}

# Runs at most one test program per available CPU, each with `args` (its tests filtered and
# skipped), in its package's directory as cargo does. Prints each one's time and each failure's
# output (with --nocapture, each that ran a test), and fails if no test ran. Up to four GPU
# tests share the GPU at a time: they check what the GPU computes, not how long it takes (the
# measurements run alone, below). A program that waits for another process's hold on the GPU
# notes how long in `gpu-waits` (`WRELA_GPU_WAITS`), for the gate's budget.
run_tests() { # args...
  python3 - "$logs" "${CARGO_TARGET_DIR:-target}/native-cache" "$@" <<'PY'
import concurrent.futures, errno, json, os, re, shutil, subprocess, sys, tempfile, time
sys.path.insert(0, "tools")
from resources import cpu_count
logs, native_cache, *args = sys.argv[1:]
native_cache = os.path.abspath(native_cache)
# Browser pages stay beneath the checkout even when Cargo's target is elsewhere.
# Keep the same depth as target/tmp: copied fixtures have relative dependency paths.
scratch_root = os.path.abspath("target")
os.makedirs(scratch_root, exist_ok=True)
cores = cpu_count()
# Software Vulkan competes for CPU time, so default to one test thread per program.
# Hardware uses up to four, bounded by available CPUs and the shared GPU limit.
test_threads = 1 if os.environ.get("WRELA_SOFTWARE_GPU") else max(1, min(4, cores))
programs = []
for line in open(f"{logs}/tests.json"):
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        programs.append(m)
# The end-to-end suite is the longest program, so the gate's time: it starts first, and the
# others yield to it (`nice`: with all at one priority, it took half as long again as alone).
programs.sort(key=lambda m: not os.path.dirname(m["manifest_path"]).endswith("compiler/tests"))
def run(index, m):
    # As cargo runs a test program: in its package's directory.
    pkg = os.path.dirname(m["manifest_path"])
    # Compiled code is kept across runs (wrela_host's cache): a test's build is made afresh
    # each run, and its WASM is mostly the same.
    scratch = tempfile.mkdtemp(prefix="test-scratch-", dir=scratch_root)
    # A concurrent Cargo build may replace its test binary. Keep this run's executable
    # reachable so tests that spawn themselves (the fuzz workers) can still find it.
    executable = os.path.join(scratch, "test-program")
    try:
        os.link(m["executable"], executable)
    except OSError as error:
        if error.errno != errno.EXDEV:
            raise
        shutil.copy2(m["executable"], executable)
    env = {**os.environ, "CARGO_MANIFEST_DIR": pkg, "WRELA_NATIVE_CACHE": native_cache,
           "WRELA_TEST_TMPDIR": scratch, "RUST_TEST_THREADS": str(test_threads),
           "WRELA_GPU_SHARED": str(max(2, min(4, cores))), "WRELA_GPU_WAITS": f"{logs}/gpu-waits"}
    name = f"{m['target']['name']} ({m['target']['kind'][0]})"
    first = pkg.endswith("compiler/tests")
    start = time.monotonic()
    with open(f"{logs}/run-{index}", "w+") as out:
        proc = subprocess.run([*([] if first else ["nice", "-n", "10"]), executable, "-q", *args],
                              cwd=pkg, env=env, stdout=out, stderr=subprocess.STDOUT)
        out.seek(0)
        text = out.read()
    if proc.returncode == 0:
        shutil.rmtree(scratch, ignore_errors=True)
    return name, proc.returncode, text, time.monotonic() - start, scratch
failed, ran = False, 0
with concurrent.futures.ThreadPoolExecutor(max_workers=cores) as pool:
    waiting = [pool.submit(run, i, m) for i, m in enumerate(programs)]
    for future in concurrent.futures.as_completed(waiting):
        name, code, text, took, scratch = future.result()
        passed = sum(int(n) for n in re.findall(r"test result: \w+\. (\d+) passed", text))
        ran += passed
        if code != 0:
            failed = True
            print(text)
            print(f"{name} failed; scratch kept at {scratch}")
        elif passed and "--nocapture" in args:
            print(text)
        elif passed and took >= 1:
            print(f"  {name}: {took:.1f}s")
print(f"  {ran} tests passed")
sys.exit(1 if failed or ran == 0 else 0)
PY
}

# Every test but the long ones and the measurements.
skips=()
for t in "${long_tests[@]}" "${measures[@]}"; do skips+=(--skip "$t"); done

# A focused edit-loop check: package selection avoids building unrelated crates; filters then
# select tests by name. With --long, the long checks run too. Nothing else runs.
if [ "${#filters[@]}" -gt 0 ] || [ "${#packages[@]}" -gt 0 ]; then
  selection=(--workspace)
  if [ "${#packages[@]}" -gt 0 ]; then
    selection=()
    for package in "${packages[@]}"; do selection+=(--package "$package"); done
    step "focused check: build tests for ${packages[*]} (release)"
  else
    step "focused check: build the tests (release)"
  fi
  cargo test -q --release "${selection[@]}" --no-run --message-format=json > "$logs/tests.json"
  if [ "$long" = 1 ]; then
    skips=()
    for t in "${measures[@]}"; do skips+=(--skip "$t"); done
  fi
  if [ "${#filters[@]}" -gt 0 ]; then
    step "focused check: tests whose names hold ${filters[*]}"
  else
    step "focused check: tests in ${packages[*]}"
  fi
  run_tests --include-ignored "${skips[@]}" "${nocapture[@]}" "${filters[@]}"
  rm -rf "$logs"
  echo "focused check passed; the full gate is still required before merge"
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

# The optional memory of slow tests that passed (tools/memo.py), where a test can be traced
# and isolated (Linux: strace, mount namespaces). --fresh, --long and --full run every test.
# WRELA_GATE_MEMO is the memory's directory, or `off`; in a Claude project's session it is in the
# project's shared folder, so the project's sessions share what they learn.
memo=off
if [ "${WRELA_GATE_MEMO:-}" = off ]; then
  echo "  test memo disabled (WRELA_GATE_MEMO=off); running every gate test"
elif python3 tools/memo.py available; then
  if [ "$fresh" = 1 ] || [ "$long" = 1 ]; then memo=fresh; else memo=on; fi
else
  echo "  test memo unavailable (requires strace and mount namespaces); running every gate test"
fi
if [ -n "${WRELA_GATE_MEMO:-}" ] && [ "$WRELA_GATE_MEMO" != off ]; then
  memo_dir=$WRELA_GATE_MEMO
elif [ -d /mnt/project-files ] && [ -w /mnt/project-files ]; then
  memo_dir=/mnt/project-files/.gate-memo
else
  memo_dir="${CARGO_TARGET_DIR:-target}/gate-memo"
fi

step "tests, on the CPU and the GPU, but the long ones and the measurements$( [ -n "${WRELA_SOFTWARE_GPU:-}" ] && [ "${#heavy_tests[@]}" -gt 0 ] && echo " (on this software GPU, the ${#heavy_tests[@]} heavy ones are long: tools/check.sh --long <name> runs one)"), with bounded CPU workers$( [ "$memo" != off ] && echo " (each slow test alone, the longest first$( [ "$memo" = on ] && echo ", unless it passed with the same inputs before"))"); beside them, the doc tests and the WGSL size budgets (every example's, sketch's and lens's pipelines)"
tests_started=$SECONDS
ps -A -o pid=,ppid=,time= > "$logs/ps-before"
side doc cargo test -q --release --workspace --doc
side wgsl python3 tools/wgsl_budgets.py "$wrela"
tests=0
if [ "$memo" = off ]; then
  run_tests --include-ignored "${skips[@]}" || tests=1
else
  python3 tools/memo.py run "$logs" "${CARGO_TARGET_DIR:-target}/native-cache" "$memo" "$memo_dir" \
    --include-ignored "${skips[@]}" || tests=1
fi
side_results
ps -A -o pid=,ppid=,time= > "$logs/ps-after"
[ "$tests" = 0 ] || exit 1
grep -h "pipelines in" "$logs/wgsl" || true
took=$((SECONDS - tests_started))
echo "  the tests took ${took}s; built and tested in $((SECONDS - started))s"
# The budget that keeps the gate quick: a test that takes seconds belongs in the long checks.
# It's an idle machine's. While another process held the GPU alone (a measurement, a test run
# outside the checks), the tests waited for it: they're over budget only if they are without
# their longest wait. And the cores other processes kept busy meanwhile (another session's
# checks, Spotlight indexing what the builds wrote: 2.5 of 10 cores on 2026-10-10) weren't the
# tests': the budget grows by the share they took.
if [ "$took" -gt 60 ]; then
  waited=0
  if [ -s "$logs/gpu-waits" ]; then
    waited=$(awk '$1 > m { m = $1 } END { printf "%d", m }' "$logs/gpu-waits")
    echo "  the tests waited for the GPU (at most ${waited}s), held alone by:"
    cut -d' ' -f2- "$logs/gpu-waits" | sort -u | sed 's/^/    /'
  fi
  budget=$(python3 - "$logs" "$$" "$took" <<'PY'
import os, sys
logs, me, took = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
def read(name):
    ps = {}
    for line in open(f"{logs}/{name}"):
        pid, ppid, t = line.split()
        parts = [float(x) for x in t.split(":")]
        secs = sum(v * 60 ** (len(parts) - 1 - i) for i, v in enumerate(parts))
        ps[int(pid)] = (int(ppid), secs)
    return ps
before, after = read("ps-before"), read("ps-after")
def ours(pid):
    while pid > 1:
        if pid == me:
            return True
        pid = after.get(pid, before.get(pid, (1, 0)))[0]
    return False
# The CPU each process outside the checks used while they ran (one that started or ended
# meanwhile isn't counted: the share is a lower bound).
other = sum(after[p][1] - before[p][1] for p in after if p in before and not ours(p))
cores = os.cpu_count()
busy = min(other / max(took, 1), cores - 1)
print(f"{busy:.1f} {60 * cores / (cores - busy):.0f}")
PY
)
  busy=${budget% *}; budget=${budget#* }
  echo "  other processes kept ${busy} of the machine's cores busy meanwhile: the budget is ${budget}s"
  if [ $((took - waited)) -gt "$budget" ]; then
    # The budget is a GPU's: on the CPU's Vulkan (a cloud machine, tools/cloud.sh), the GPU
    # tests take many times as long. It's reported there, not gated.
    if [ -n "${WRELA_SOFTWARE_GPU:-}" ]; then
      echo "  over the budget (${budget}s), on a software GPU (WRELA_SOFTWARE_GPU): reported, not gated"
    else
      echo "the tests took ${took}s, over their budget (${budget}s): tag the slow ones \`#[ignore = \"long: ...\"]\`" >&2
      exit 1
    fi
  else
    echo "  within the budget for the cores the tests had: not a failure"
  fi
fi

if [ "$long" = 1 ]; then
  # A new seed each run, so the runs between them cover more cases; a failure names its seed.
  seed=$(( (RANDOM << 15 | RANDOM) + 1 ))
  side_jobs=()
  step "long: the long checks$( [ "$full" = 1 ] && echo ", at full size"), sharing the GPU (one headless Chrome at a time); beside them, the grammar's 10^6 generated programs against the oracle and the formatter, and 200,000 GBNF samples (seed $seed)"
  side differential cargo run -q --release -p wrela-grammar --bin differential -- 1000000 --seed "$seed"
  side wgsl_baked python3 tools/wgsl_budgets.py "$wrela" --baked
  side gbnf cargo run -q --release -p wrela-grammar --bin gbnf-sample -- 200000 --seed "$seed"
  # With --full, at full size.
  if [ "$full" = 1 ]; then export WRELA_FULL=1; fi
  run_tests --ignored --exact "${long_tests[@]}" || tests=1
  unset WRELA_FULL
  side_results
  [ "$tests" = 0 ] || exit 1
  grep -h "largest pipeline" "$logs/wgsl_baked" || true

  fuzz=$( [ "$full" = 1 ] && echo 1000000 || echo 20000 )
  step "long: fuzz: $fuzz mutated programs of tiers 1 and 2, one worker per core (seed $seed)"
  WRELA_FUZZ_ITERS=$fuzz WRELA_FUZZ_TIERS=12 WRELA_FUZZ_SEED=$seed \
    cargo test -q --release -p wrela-tests --test suite fuzz::

  # Hardware timing evidence runs with --full on the reference machine. It was only
  # reported here, never gated, and can take hours on software Vulkan.

fi

if [ "$full" = 1 ]; then
  step "full: every test of the gate at full size"
  WRELA_FULL=1 run_tests --include-ignored "${skips[@]}"

  step "full: the CLI's speed, cold processes: hello field's check < 200 ms and build < 2 s, sketch 03's, the herd's, the clearing's, the floor's (its bake cached) and the lens's (on each subject) < 500 ms and < 5 s"
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
        # The floor with its bake from the constants' cache (#55 AC16).
        ("check the floor", ["check", "examples/last-green"], 0.5),
        ("build the floor", ["build", "examples/last-green", "-o", f"{out}/speed-floor"], 5.0),
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
  # fits' 10 s, and an edit's 2 s to show. So do the herd's and the grazer's ratios to spike 01
  # (#42): each compares two timings taken side by side, and another test's load skews one of them.
  step "full: the measurements, one at a time"
  cargo test -q --release --workspace -- --ignored --exact --test-threads=1 "${measures[@]}"
fi
rm -rf "$logs"

printf '\nall checks passed\n'
