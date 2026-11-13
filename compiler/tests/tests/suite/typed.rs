//! The compiler checks itself on random well-typed programs (`wrela_tests::typed`): each oracle
//! over 32 programs from a fixed seed (16 for the GPU's), or `WRELA_TYPED_SEEDS` of them. A
//! failure is shrunk and written to `target/typed/<oracle>-<seed>.wrela`, and the test panics
//! with the seed, the file and what failed. The soak (`typed-soak`) runs the same over many
//! more; each bug it found is a file in `compiler/tests/typed/regress/`.

use wrela_tests::typed::Mode;
use wrela_tests::typed::oracles::{self, Counts, Oracle};

/// The programs' base seed.
const BASE: u64 = 0x7e57_ed00;

/// How many programs an oracle sees: `default`, or `WRELA_TYPED_SEEDS`.
fn count(default: u64) -> u64 {
    std::env::var("WRELA_TYPED_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

/// Runs `oracle` over `n` programs of each mode it takes: the counts, summed. Panics with
/// every failure.
fn over(oracle: Oracle, n: u64) -> Counts {
    let scratch = crate::scratch(&format!("typed-{}", oracle.name()));
    let out = wrela_tests::repo_root().join("target/typed");
    let runs: Vec<(Mode, u64)> = oracle
        .modes()
        .iter()
        .flat_map(|&m| (0..n).map(move |i| (m, oracles::program_seed(BASE, i))))
        .collect();
    let each = |&(mode, seed): &(Mode, u64)| {
        oracles::check_seed(oracle, mode, oracle.features(), seed, &scratch, &out)
    };
    // The GPU's oracle runs one program at a time: each opens its own device.
    let results = if matches!(oracle, Oracle::O6 | Oracle::O7 | Oracle::O10) {
        runs.iter().map(each).collect()
    } else {
        wrela_tests::par_map(&runs, each)
    };
    let mut total = Counts::default();
    let mut failures = Vec::new();
    for r in results {
        match r {
            Ok(c) => {
                total.points += c.points;
                total.counted += c.counted;
            }
            Err(why) => failures.push(why),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} programs fail:\n{}",
        failures.len(),
        runs.len(),
        failures.join("\n")
    );
    println!(
        "{}: {} programs, {} points, {} counted",
        oracle.name(),
        runs.len(),
        total.points,
        total.counted
    );
    total
}

/// O1: every program, tame or wild, builds and runs at 16 points without a trap.
#[test]
fn o1_programs_build_and_run() {
    over(Oracle::O1, count(32));
}

/// O2: the interval holds every sampled value.
#[test]
fn o2_intervals_enclose_the_values() {
    over(Oracle::O2, count(32));
}

/// O3: the gradient agrees with central differences where the function is smooth, and at least
/// 30% of points are smooth (or the generator is degenerate).
#[test]
fn o3_gradients_agree_with_central_differences() {
    let c = over(Oracle::O3, count(32));
    assert!(
        f64::from(c.counted) >= 0.3 * f64::from(c.points),
        "only {} of {} points are smooth enough to compare",
        c.counted,
        c.points
    );
}

/// O4: a tape agrees with the function it was made from (§14.4).
#[test]
fn o4_tapes_agree_with_their_functions() {
    over(Oracle::O4, count(32));
}

/// O5: an optimized build gives the same bits as one with no optimization.
#[test]
fn o5_the_optimizer_keeps_the_bits() {
    over(Oracle::O5, count(32));
}

/// O6: the GPU's value lies in its own interval over the point, and the CPU's and the GPU's are
/// apart by at most the two point intervals' widths.
#[test]
#[ignore = "needs a GPU"]
fn o6_the_targets_agree() {
    over(Oracle::O6, count(16));
}

/// Regression files the oracles still fail on, by file name: each a compiler bug the typed
/// fuzzer found and this release didn't fix (the pull request lists them).
const KNOWN: &[&str] = &[];

/// Each file in `compiler/tests/typed/regress/` (but `KNOWN`) through the oracle its first line
/// names, at the seed its header gives.
#[test]
fn every_regression_still_holds() {
    let dir = wrela_tests::repo_root().join("compiler/tests/typed/regress");
    let files: Vec<_> = wrela_tests::cases(&dir)
        .into_iter()
        .filter(|f| !KNOWN.iter().any(|k| f.file_name().is_some_and(|n| n == *k)))
        .collect();
    assert!(!files.is_empty(), "no regression files in {}", dir.display());
    let scratch = crate::scratch("typed-regress");
    let failures: Vec<String> =
        files.iter().filter_map(|f| oracles::run_file(f, &scratch).err()).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// O7: the GPU code, interpreted (`wrela_ir::interp`), gives what the GPU gave.
#[test]
#[ignore = "needs a GPU"]
fn o7_the_interpreter_agrees_with_the_gpu() {
    over(Oracle::O7, count(16));
}

/// O8: reverse mode's gradient agrees with forward mode's.
#[test]
fn o8_reverse_gradients_agree_with_forward_ones() {
    over(Oracle::O8, count(32));
}

/// O9: the error walk (`wrela_ir::interp::error`) is sound: moving up to three values of `f`
/// within their bounds moves it by no more than the walk's bound, at 256 points of a box.
#[test]
fn o9_the_error_walk_bounds_moved_values() {
    over(Oracle::O9, count(32));
}

/// O10 (stage K.3): an integer program as a `@deterministic` kernel gives the same bits on the
/// GPU as its CPU form (`k.run`), at every one of 1,024 places.
#[test]
#[ignore = "needs a GPU"]
fn o10_exact_kernels_agree_with_their_cpu_form() {
    over(Oracle::O10, count(16));
}
