//! Kernels the same on every GPU (language.md §18.14, plan stage K): compiler/tests/exact runs
//! three `@deterministic` kernels (Life, a hash mix, a read past the input) on the GPU over
//! 10,000 random inputs each and on the CPU (`k.run`), and prints how many places agree. K.2:
//! `run` is Life as written by hand, and a read past the input is its last element (the
//! package's tests). K.3, G-agree: every place agrees, bit for bit, on the native host's GPU and
//! in Chrome.

use crate::built;
use wrela_host::Host;

/// The `exact <kernel> <agree> of <n>` lines a run printed: each kernel's counts.
fn agreements(lines: &[String]) -> Vec<(String, u64, u64)> {
    lines
        .iter()
        .filter_map(|l| {
            let rest = l.split("exact ").nth(1)?;
            let w: Vec<&str> = rest.split_whitespace().collect();
            Some((w.first()?.to_string(), w.get(1)?.parse().ok()?, w.get(3)?.parse().ok()?))
        })
        .collect()
}

#[test]
fn run_is_life_by_hand_and_reads_past_the_input_at_its_end() {
    let dir = crate::scratch("exact-tests");
    let src = wrela_tests::repo_root().join("compiler/tests/exact");
    for f in ["wrela.toml", "main.wrela"] {
        std::fs::copy(src.join(f), dir.join(f)).expect("copy");
    }
    assert_eq!(crate::tests_pass(&dir), 2);
}

#[test]
#[ignore = "needs a GPU"]
fn the_gpu_and_run_agree_bit_for_bit() {
    let mut host = Host::load(built("exact")).expect("load");
    let times: Vec<f32> = (0..30).map(|i| i as f32 / 60.0).collect();
    host.run_frames(&times, 16, 16).expect("run");
    let lines = host.take_logs();
    let got = agreements(&lines);
    eprintln!("{got:?} on {}", host.adapter());
    assert_eq!(got.len(), 3, "{lines:?}");
    for (k, agree, n) in got {
        assert_eq!(agree, n, "{k}: {agree} of {n} places agree");
    }
}

#[test]
#[ignore = "needs Chrome and a GPU"]
fn in_chrome_the_gpu_and_run_agree_bit_for_bit() {
    let (_, rel) = wrela_tests::page("compiler/tests/exact", "exact-page");
    let run = wrela_tests::run_in_chrome(&rel, 30, 16, 16, 60.0);
    let lines: Vec<String> = run.printed.iter().map(|(_, l)| l.clone()).collect();
    let got = agreements(&lines);
    eprintln!("{got:?} in Chrome");
    assert_eq!(got.len(), 3, "{lines:?}");
    for (k, agree, n) in got {
        assert_eq!(agree, n, "{k}: {agree} of {n} places agree");
    }
}

/// Stage K.6's ceiling, a measurement (it prints and asserts nothing of the times): a 1,024 x
/// 1,024 Life step by `run`, against what a tick with a held answer must still do, its inputs
/// hashed by std's `Hasher` (the key) and the answer copied into `out`. A held answer can make a
/// tick at most this much faster; the gate asks for 3 times.
#[test]
#[ignore = "a measurement"]
fn k6_the_most_a_held_answer_could_save() {
    let dir = crate::built("exact_ceiling");
    let mut host = wrela_host::CpuBuild::load(&dir).unwrap().start_with(1).unwrap();
    let mut ms = Vec::new();
    for name in ["run_once", "take_once"] {
        let _ = host.call_export(name, &[]).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..20 {
            host.call_export(name, &[]).unwrap();
        }
        ms.push(t.elapsed().as_secs_f64() * 1e3 / 20.0);
    }
    eprintln!(
        "run {:.2} ms, hash and copy {:.2} ms: at most {:.2} times",
        ms[0],
        ms[1],
        ms[0] / ms[1]
    );
}
