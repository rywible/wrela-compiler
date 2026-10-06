#!/usr/bin/env bash
# The local gate: what must pass before a commit lands on main. There's no CI (by design).
#
#   tools/check.sh           formatting, lints, every test that needs neither a GPU nor Chrome,
#                            the tools' tests, the browser runtime's checks, a fuzz smoke test,
#                            the CLI's speed budgets and the WGSL size budgets
#   tools/check.sh --gpu     also the GPU and headless-Chrome tests (they queue on the GPU lock)
#   tools/check.sh --long    also the full grammar run (10^6 programs) and 10^6 fuzz cases
#
# Needs: the Rust toolchain in rust-toolchain.toml, bun, python3; Chrome stable for --gpu.
set -euo pipefail
cd "$(dirname "$0")/.."

# The gate runs everything, the same way each time: a variable that narrows the tests, rewrites
# goldens or picks a fuzz seed (left set in the shell by an earlier run) doesn't apply here.
unset WRELA_RUN_ONLY WRELA_BLESS WRELA_FUZZ_ITERS WRELA_FUZZ_SEED WRELA_FUZZ_WORKER WRELA_FUZZ_JOBS \
  WRELA_FUZZ_TIERS

gpu=0
long=0
for arg in "$@"; do
  case "$arg" in
    --gpu) gpu=1 ;;
    --long) long=1 ;;
    *) echo "usage: tools/check.sh [--gpu] [--long]" >&2; exit 2 ;;
  esac
done

step() { printf '\n== %s\n' "$*"; }

# wrela sources the formatter owns. The conformance and diagnostics cases aren't here: some
# are malformed on purpose, and their annotations are part of their layout.
WRELA_SOURCES=(examples engine compiler/std compiler/tests/fields compiler/tests/math compiler/tests/sketches
  compiler/tests/numerics compiler/tests/render compiler/tests/queries compiler/tests/simd
  compiler/tests/input compiler/tests/lift ui studio)

step "rustfmt"
cargo fmt --all -- --check

step "clippy (warnings are errors)"
cargo clippy -q --workspace --all-targets -- -D warnings

step "build the CLI (release)"
cargo build -q --release -p wrela
wrela="${CARGO_TARGET_DIR:-target}/release/wrela"

step "wrela fmt --check"
"$wrela" fmt --check "${WRELA_SOURCES[@]}"

step "tests at full size: unit, conformance, diagnostics goldens, grammar, derived interpretations, ABI goldens, reproducibility, fuzz"
WRELA_FULL=1 cargo test -q --workspace

step "tools: the dev server and the headless-Chrome driver (fake Chrome)"
python3 -m unittest discover -q -s tools/tests

step "browser runtime: tests, types, dist is current, size budget"
(cd runtime/browser && bun run checks)

step "speed, cold processes: hello field's check < 200 ms and build < 2 s, sketch 03's and the lens's (on each subject) < 500 ms and < 5 s; WGSL size of every example's, sketch's and lens's pipelines"
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
    # Each pipeline's WGSL: what the browser downloads and the driver compiles (pipeline
    # creation time is budgeted by the --gpu tests). Hello field keeps its own budget; every
    # other example's and sketch's pipelines are at most 256 KiB each.
    packages = sorted(p.parent for root in ["examples", "compiler/tests/sketches", "ui/tests"]
                      for p in pathlib.Path(root).glob("*/main.wrela"))
    packages += [pathlib.Path(lens(s)) for s in subjects]
    pipelines = 0
    for pkg in packages:
        built = pathlib.Path(out) / pkg.parent.parent.parent.name / pkg.name if pkg.name == "lens" else pathlib.Path(out) / pkg.name
        lift = ["--lift", pkg.parent.parent.parent.name] if pkg.name == "lens" else []
        subprocess.run([wrela, "build", str(pkg), "-o", str(built), *lift], check=True, capture_output=True)
        raw_budget, gz_budget = (128 * 1024, 24 * 1024) if str(pkg) == hello else (256 * 1024, None)
        for wgsl in sorted(built.glob("*.wgsl")):
            pipelines += 1
            text = wgsl.read_bytes()
            raw, packed = len(text), len(gzip.compress(text))
            if raw > raw_budget or (gz_budget and packed > gz_budget):
                sys.exit(f"{pkg}/{wgsl.name} is {raw / 1024:.0f} KiB ({packed / 1024:.0f} gzipped), over its size budget")
        largest = max((w.stat().st_size for w in built.glob("*.wgsl")), default=0)
        print(f"  {pkg}: largest pipeline {largest / 1024:.0f} KiB (budget {raw_budget // 1024})")
    print(f"  {pipelines} pipelines in {len(packages)} packages")
finally:
    shutil.rmtree(out, ignore_errors=True)
PY

if [ "$gpu" = 1 ]; then
  # The time budgets run alone: the load-time ones measure shader compilation on the CPU (with
  # the other tests compiling beside them, spike 13's cold load took 2.6 s instead of 0.8 s),
  # and Chrome's input latency and text budgets measure real time (under the other tests'
  # load, an event missed its next frame, and 10,000 glyphs took 0.52 ms instead of 0.23).
  # So do the lens's: its frames' and clicks' times in Chrome, its drag updates in a session,
  # its fits' 10 s, and an edit's 2 s to show.
  budgets=(loading_creates_the_pipelines_within_budget the_gpu_certificate_is_small_quick_and_right
    events_reach_the_program_by_the_next_frame a_screen_of_code_draws_within_half_a_millisecond
    the_lens_is_fast_enough_and_the_same_in_both_hosts a_session_gives_the_same_results_headless_and_in_chrome
    fits_recover_the_wolfs_longer_neck_and_bigger_ears an_edit_by_another_tool_shows_in_the_open_lens_within_2_s)
  skips=()
  for b in "${budgets[@]}"; do skips+=(--skip "$b"); done
  step "GPU and headless-Chrome tests"
  cargo test -q --release --workspace -- --ignored "${skips[@]}"
  step "time budgets, one test at a time"
  cargo test -q --release -p wrela-tests --test suite -- --ignored --test-threads=1 "${budgets[@]}"
fi

if [ "$long" = 1 ]; then
  step "grammar: 10^6 generated programs against the oracle and the formatter"
  cargo run -q --release -p wrela-grammar --bin differential -- 1000000
  step "GBNF: 200,000 sampled programs"
  cargo run -q --release -p wrela-grammar --bin gbnf-sample -- 200000
  step "fuzz: 10^6 mutated programs of tiers 1 and 2, one worker per core (about an hour)"
  WRELA_FUZZ_ITERS=1000000 WRELA_FUZZ_TIERS=12 cargo test -q --release -p wrela-tests --test suite fuzz::
fi

printf '\nall checks passed\n'
