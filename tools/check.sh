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
WRELA_SOURCES=(examples compiler/std compiler/tests/fields compiler/tests/math compiler/tests/sketches
  compiler/tests/numerics compiler/tests/render compiler/tests/queries compiler/tests/simd)

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

step "speed, cold processes: hello field's check < 200 ms and build < 2 s, sketch 03's < 500 ms and < 5 s; WGSL size of every example's and sketch's pipelines"
python3 - "$wrela" <<'PY'
import gzip, pathlib, shutil, subprocess, sys, tempfile, time
wrela = sys.argv[1]
def best(args, n=5):
    times = []
    for _ in range(n):
        t = time.perf_counter()
        subprocess.run([wrela, *args], check=True, capture_output=True)
        times.append(time.perf_counter() - t)
    return min(times), max(times)
out = tempfile.mkdtemp()
try:
    hello, sketch = "examples/hello-field", "compiler/tests/sketches/03-simulation"
    for name, args, budget in [
        ("check hello field", ["check", hello], 0.2),
        ("build hello field", ["build", hello, "-o", f"{out}/speed-hello"], 2.0),
        ("check sketch 03", ["check", sketch], 0.5),
        ("build sketch 03", ["build", sketch, "-o", f"{out}/speed-sketch"], 5.0),
    ]:
        lo, hi = best(args)
        print(f"  wrela {name}: {lo * 1000:.0f}-{hi * 1000:.0f} ms (budget {budget * 1000:.0f} ms)")
        if hi > budget:
            sys.exit(f"wrela {name} took {hi * 1000:.0f} ms, over its budget")
    # Each pipeline's WGSL: what the browser downloads and the driver compiles (pipeline
    # creation time is budgeted by the --gpu tests). Hello field keeps its own budget; every
    # other example's and sketch's pipelines are at most 256 KiB each.
    packages = sorted(p.parent for root in ["examples", "compiler/tests/sketches"]
                      for p in pathlib.Path(root).glob("*/main.wrela"))
    pipelines = 0
    for pkg in packages:
        built = pathlib.Path(out) / pkg.name
        subprocess.run([wrela, "build", str(pkg), "-o", str(built)], check=True, capture_output=True)
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
  # The load-time budgets measure shader compilation on the CPU, so they run alone: with the
  # other tests compiling beside them, spike 13's cold load took 2.6 s instead of 0.8 s.
  budgets=(loading_creates_the_pipelines_within_budget the_gpu_certificate_is_small_quick_and_right)
  step "GPU and headless-Chrome tests"
  cargo test -q --release --workspace -- --ignored --skip "${budgets[0]}" --skip "${budgets[1]}"
  step "load-time budgets, one test at a time"
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
