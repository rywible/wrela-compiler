#!/usr/bin/env bash
# The local gate: what must pass before a commit lands on main. There's no CI (by design).
#
#   tools/check.sh           formatting, lints, every test that needs neither a GPU nor Chrome,
#                            the tools' tests, the browser runtime's checks, a fuzz smoke test,
#                            and the CLI's speed budget
#   tools/check.sh --gpu     also the GPU and headless-Chrome tests (they queue on the GPU lock)
#   tools/check.sh --long    also the full grammar run (10^6 programs) and a longer fuzz
#
# Needs: the Rust toolchain in rust-toolchain.toml, bun, python3; Chrome stable for --gpu.
set -euo pipefail
cd "$(dirname "$0")/.."

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
WRELA_SOURCES=(examples compiler/std compiler/tests/fields compiler/tests/math
  compiler/tests/numerics compiler/tests/render)

step "rustfmt"
cargo fmt --all -- --check

step "clippy (warnings are errors)"
cargo clippy -q --workspace --all-targets -- -D warnings

step "build the CLI (release)"
cargo build -q --release -p wrela
wrela=target/release/wrela

step "wrela fmt --check"
"$wrela" fmt --check "${WRELA_SOURCES[@]}"

step "tests at full size: unit, conformance, diagnostics goldens, grammar, derived interpretations, ABI goldens, reproducibility, fuzz"
WRELA_FULL=1 cargo test -q --workspace

step "tools: the dev server and the headless-Chrome driver (fake Chrome)"
python3 -m unittest discover -q -s tools/tests

step "browser runtime: tests, types, dist is current, size budget"
(cd runtime/browser && bun run checks)

step "speed: check < 200 ms and build < 2 s, cold processes; WGSL size (examples/hello-field)"
python3 - "$wrela" <<'PY'
import gzip, pathlib, subprocess, sys, tempfile, time
wrela = sys.argv[1]
def best(args, n=5):
    times = []
    for _ in range(n):
        t = time.perf_counter()
        subprocess.run([wrela, *args], check=True, capture_output=True)
        times.append(time.perf_counter() - t)
    return min(times), max(times)
out = tempfile.mkdtemp()
for name, args, budget in [("check", ["check", "examples/hello-field"], 0.2),
                           ("build", ["build", "examples/hello-field", "-o", out], 2.0)]:
    lo, hi = best(args)
    print(f"  wrela {name}: {lo * 1000:.0f}-{hi * 1000:.0f} ms (budget {budget * 1000:.0f} ms)")
    if hi > budget:
        sys.exit(f"wrela {name} took {hi * 1000:.0f} ms, over its budget")
# Each pipeline's WGSL: what the browser downloads and the driver compiles (pipeline creation
# time is budgeted by the --gpu tests).
RAW, GZIPPED = 128 * 1024, 24 * 1024
for wgsl in sorted(pathlib.Path(out).glob("*.wgsl")):
    text = wgsl.read_bytes()
    raw, packed = len(text), len(gzip.compress(text))
    print(f"  {wgsl.name}: {raw / 1024:.0f} KiB, {packed / 1024:.0f} KiB gzipped (budget {RAW // 1024}, {GZIPPED // 1024})")
    if raw > RAW or packed > GZIPPED:
        sys.exit(f"{wgsl.name} is over its size budget")
PY

if [ "$gpu" = 1 ]; then
  step "GPU and headless-Chrome tests"
  cargo test -q --release --workspace -- --ignored
fi

if [ "$long" = 1 ]; then
  step "grammar: 10^6 generated programs against the oracle and the formatter"
  cargo run -q --release -p wrela-grammar --bin differential -- 1000000
  step "GBNF: 200,000 sampled programs"
  cargo run -q --release -p wrela-grammar --bin gbnf-sample -- 200000
  step "fuzz: 50,000 mutated programs"
  WRELA_FUZZ_ITERS=50000 cargo test -q --release -p wrela-tests --test suite fuzz::
fi

printf '\nall checks passed\n'
