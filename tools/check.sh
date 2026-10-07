#!/usr/bin/env bash
# The local gate: what must pass before a commit lands on main. There's no CI (by design).
#
#   tools/check.sh          formatting, lints, and every test but the long ones, on the CPU and
#                           the GPU (release builds): unit, conformance, diagnostics goldens, run
#                           and GPU tests, grammar and fuzz samples; the tools' tests; the
#                           browser runtime's checks; the WGSL size budgets. A minute or two at
#                           most on a warm build, and it says if its tests took longer.
#   tools/check.sh --long   also the long checks: the tests whose `#[ignore]` reason starts
#                           `long:` (headless Chrome, timing budgets, soak runs, comparisons with
#                           spike 01), the tests at full size, the CLI's speed budgets, the full
#                           grammar run (10^6 programs) and 10^6 fuzz cases. An hour or more.
#
# Needs: the Rust toolchain in rust-toolchain.toml, bun, python3, a GPU; Chrome stable for --long.
set -euo pipefail
cd "$(dirname "$0")/.."

# The gate runs everything, the same way each time: a variable that narrows the tests, rewrites
# goldens or picks a fuzz seed (left set in the shell by an earlier run) doesn't apply here.
unset WRELA_RUN_ONLY WRELA_BLESS WRELA_FUZZ_ITERS WRELA_FUZZ_SEED WRELA_FUZZ_WORKER WRELA_FUZZ_JOBS \
  WRELA_FUZZ_TIERS WRELA_FULL

long=0
for arg in "$@"; do
  case "$arg" in
    --long) long=1 ;;
    *) echo "usage: tools/check.sh [--long]" >&2; exit 2 ;;
  esac
done

step() { printf '\n== %s\n' "$*"; }

# The long tests, by name (`module::test`, or `test` in a test program of one file): each test
# whose `#[ignore]` reason starts `long:`.
long_tests() {
  python3 - <<'PY'
import pathlib, re
for f in sorted([*pathlib.Path("compiler").rglob("*.rs"), *pathlib.Path("runtime").rglob("*.rs")]):
    if "target" in f.parts:
        continue
    lines = f.read_text().splitlines()
    for i, line in enumerate(lines):
        if re.search(r'#\[ignore = "long:', line):
            name = next(re.search(r"fn (\w+)\(", l).group(1) for l in lines[i + 1:] if re.search(r"fn (\w+)\(", l))
            # A test file at the top of `tests/` is a test program of its own (no module).
            print(name if f.parent.name == "tests" else f"{f.stem}::{name}")
PY
}

# wrela sources the formatter owns. The conformance and diagnostics cases aren't here: some
# are malformed on purpose, and their annotations are part of their layout.
WRELA_SOURCES=(examples engine compiler/std compiler/tests/fields compiler/tests/math compiler/tests/sketches
  compiler/tests/numerics compiler/tests/render compiler/tests/queries compiler/tests/simd
  compiler/tests/input compiler/tests/lift ui studio)

step "rustfmt"
cargo fmt --all -- --check

step "clippy (warnings are errors)"
cargo clippy -q --workspace --all-targets -- -D warnings

step "build the CLI and the tests (release)"
cargo build -q --release -p wrela
logs=$(mktemp -d)
cargo test -q --release --workspace --no-run --message-format=json > "$logs/tests.json"
wrela="${CARGO_TARGET_DIR:-target}/release/wrela"

step "wrela fmt --check"
"$wrela" fmt --check "${WRELA_SOURCES[@]}"

step "tests, on the CPU and the GPU, but the long ones, every test program at once; beside them, the doc tests, the tools' tests (the dev server and the headless-Chrome driver, with a fake Chrome), the browser runtime's (tests, types, dist is current, size budget), and the WGSL size budgets (every example's, sketch's and lens's pipelines)"
started=$SECONDS
side() { # name, command...: runs beside the tests, its output and status kept
  local name=$1; shift
  ("$@" > "$logs/$name" 2>&1; echo $? > "$logs/$name.status") &
}
side doc cargo test -q --release --workspace --doc
side tools python3 -m unittest discover -q -s tools/tests
side bun sh -c 'cd runtime/browser && bun run checks'
side wgsl python3 tools/wgsl_budgets.py "$wrela"
long_tests > "$logs/long"
python3 - "$logs" "${CARGO_TARGET_DIR:-target}/native-cache" <<'PY'
import json, os, subprocess, sys, time
logs, native_cache = sys.argv[1], os.path.abspath(sys.argv[2])
skips = [a for t in open(f"{logs}/long").read().split() for a in ("--skip", t)]
runs = []
for line in open(f"{logs}/tests.json"):
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        # As cargo runs a test program: in its package's directory.
        pkg = os.path.dirname(m["manifest_path"])
        # Compiled code is kept across runs (wrela_host's cache): a test's build is made afresh
        # each run, and its WASM is mostly the same.
        env = {**os.environ, "CARGO_MANIFEST_DIR": pkg, "WRELA_NATIVE_CACHE": native_cache}
        out = open(f"{logs}/run-{len(runs)}", "w+")
        name = f"{m['target']['name']} ({m['target']['kind'][0]})"
        proc = subprocess.Popen([m["executable"], "-q", "--include-ignored", *skips], cwd=pkg, env=env,
                                stdout=out, stderr=subprocess.STDOUT)
        runs.append((name, proc, out, time.monotonic()))
failed = False
waiting = list(runs)
while waiting:
    time.sleep(0.05)
    for run in [r for r in waiting if r[1].poll() is not None]:
        waiting.remove(run)
        name, proc, out, start = run
        out.seek(0)
        if proc.returncode != 0:
            failed = True
            print(out.read())
            print(f"{name} failed")
        else:
            print(f"  {name}: {time.monotonic() - start:.1f}s")
sys.exit(1 if failed else 0)
PY
wait
for job in doc tools bun wgsl; do
  if [ "$(cat "$logs/$job.status")" != 0 ]; then cat "$logs/$job"; echo "the $job checks failed" >&2; exit 1; fi
done
grep -h "pipelines in" "$logs/wgsl" || true
rm -rf "$logs"
took=$((SECONDS - started))
echo "  the tests took ${took}s"
# The budget that keeps the gate quick: a test that takes seconds belongs in the long checks.
if [ "$took" -gt 60 ]; then
  echo "the tests took ${took}s, over their 60 s budget: tag the slow ones \`#[ignore = \"long: ...\"]\`" >&2
  exit 1
fi

if [ "$long" = 1 ]; then
  step "long: tests at full size"
  WRELA_FULL=1 cargo test -q --release --workspace

  step "long: the CLI's speed, cold processes: hello field's check < 200 ms and build < 2 s, sketch 03's, the herd's and the lens's (on each subject) < 500 ms and < 5 s"
  python3 - "$wrela" <<'PY'
import gzip, pathlib, shutil, subprocess, sys, tempfile, time
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
    # The lens on each subject (AC12 of #39): `wrela studio` writes its program beside the
    # subject (build/studio/lens) and builds it lifted, then keeps its compiled code.
    subjects = ["wolf", "grazer"]
    for s in subjects:
        subprocess.run([wrela, "studio", f"examples/{s}", "build"], check=True, capture_output=True)
    lens = lambda s: f"examples/{s}/build/studio/lens"
    runs = [
        ("check hello field", ["check", hello], 0.2),
        ("build hello field", ["build", hello, "-o", f"{out}/speed-hello"], 2.0),
        ("check sketch 03", ["check", sketch], 0.5),
        ("build sketch 03", ["build", sketch, "-o", f"{out}/speed-sketch"], 5.0),
        ("check the herd", ["check", "examples/herd"], 0.5),
        ("build the herd", ["build", "examples/herd", "-o", f"{out}/speed-herd"], 5.0),
    ]
    for s in subjects:
        runs += [
            (f"check the lens on the {s}", ["check", lens(s)], 0.5),
            (f"build the lens on the {s}", ["build", lens(s), "--lift", s, "-o", f"{out}/speed-lens-{s}"], 5.0),
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

  # The time budgets run alone: the load-time ones measure shader compilation on the CPU (with
  # the other tests compiling beside them, spike 13's cold load took 2.6 s instead of 0.8 s),
  # and Chrome's input latency and text budgets measure real time (under the other tests'
  # load, an event missed its next frame, and 10,000 glyphs took 0.52 ms instead of 0.23).
  # So do the lens's: its frames' and clicks' times in Chrome, its drags in a session, its
  # fits' 10 s, and an edit's 2 s to show. So do the herd's ratios to spike 01 (#42): each
  # compares two timings taken side by side, and another test's load skews one of them.
  budgets=(loading_creates_the_pipelines_within_budget the_gpu_certificate_is_small_quick_and_right
    events_reach_the_program_by_the_next_frame a_screen_of_code_draws_within_half_a_millisecond
    the_lens_is_fast_enough_and_the_same_in_both_hosts a_session_gives_the_same_results_headless_and_in_chrome
    fits_recover_the_wolfs_longer_neck_and_bigger_ears an_edit_by_another_tool_shows_in_the_open_lens_within_2_s
    the_cpu_side_costs_what_the_spikes_did realization_matches_the_spikes
    the_herds_grazer_field_costs_what_field_wgsl_does drawing_the_herd_costs_what_the_spikes_did
    the_herd_keeps_its_frames_in_chrome moving_to_the_herd_keeps_the_frames_in_chrome
    lod_looks_like_the_finest_and_costs_like_the_coarsest the_herds_pipelines_load_cold_as_fast
    frames_dont_wait_for_slow_ticks a_closure_query_costs_what_the_hand_written_loop_does_on_the_cpu
    every_page_takes_under_a_tenth_of_a_second)
  long_names=()
  while read -r t; do long_names+=("$t"); done < <(long_tests)
  skips=()
  for b in "${budgets[@]}"; do skips+=(--skip "$b"); done
  step "long: headless Chrome, soak runs and comparisons with spike 01"
  cargo test -q --release --workspace -- --ignored "${long_names[@]}" "${skips[@]}"
  step "long: time budgets, one test at a time"
  cargo test -q --release --workspace -- --ignored --test-threads=1 "${budgets[@]}"

  step "long: grammar: 10^6 generated programs against the oracle and the formatter"
  cargo run -q --release -p wrela-grammar --bin differential -- 1000000
  step "long: GBNF: 200,000 sampled programs"
  cargo run -q --release -p wrela-grammar --bin gbnf-sample -- 200000
  step "long: fuzz: 10^6 mutated programs of tiers 1 and 2, one worker per core (about an hour)"
  WRELA_FUZZ_ITERS=1000000 WRELA_FUZZ_TIERS=12 cargo test -q --release -p wrela-tests --test suite fuzz::
fi

printf '\nall checks passed\n'
