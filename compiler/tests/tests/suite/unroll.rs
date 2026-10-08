//! Loops on the GPU written out (compiler/ir/src/unroll.rs): compiler/tests/unroll's kernels.
//! A loop of a few passes counted from a constant to a constant is written out; one whose end
//! isn't a constant, one left early and one of more passes than the limit stay loops. Each
//! kernel's 64 results, summed, are what the CPU's arithmetic gives.
//! `cargo test -p wrela-tests --test suite unroll:: -- --ignored`.

use wrela_host::{Host, Value, frame_time};
use wrela_tests::page;

#[test]
#[ignore = "needs a GPU"]
fn small_counted_loops_are_written_out_and_give_the_same_results() {
    let (dir, _) = page("compiler/tests/unroll", "unroll-native");
    // The loops left in each kernel's WGSL.
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
    let manifest = wrela_abi::Manifest::parse(&manifest).expect("parses");
    let loops = |name: &str| {
        let p = manifest.pipelines.iter().find(|p| p.name == name).expect(name);
        let wgsl = std::fs::read_to_string(dir.join(&p.shader)).expect("wgsl");
        wgsl.matches("loop {").count()
    };
    assert_eq!(loops("grid"), 0, "a 3 × 3 loop of constants is written out");
    assert_eq!(loops("up_to"), 1, "a loop to a value the invocation has stays");
    assert_eq!(loops("first_over"), 1, "a loop left early stays");
    assert_eq!(loops("long"), 1, "twenty passes stay a loop");

    let mut host = Host::load(&dir).expect("load");
    let times: Vec<f32> = (0..4).map(|i| frame_time(i, 60.0)).collect();
    host.run_frames(&times, 16, 16).expect("run");
    let mut sum = |k: u32| match host.call_export("sum", &[Value::I32(k as i32)]).expect("sum")[..]
    {
        [Value::I32(v)] => v as u32,
        ref other => panic!("sum({k}) returned {other:?}"),
    };
    let ids = 0..64u32;
    let grid: u32 = ids.clone().map(|id| 36 * (id + 1)).sum();
    let up_to: u32 = ids.clone().map(|id| (0..id % 5).sum::<u32>()).sum();
    let first_over: u32 = ids.clone().map(|id| (0..8).find(|i| i * i > id).unwrap_or(99)).sum();
    let long: u32 = ids.map(|id| (0..20).map(|i| i ^ id).sum::<u32>()).sum();
    assert_eq!(sum(0), grid, "grid");
    assert_eq!(sum(1), up_to, "up_to");
    assert_eq!(sum(2), first_over, "first_over");
    assert_eq!(sum(3), long, "long");
}
