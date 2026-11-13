//! Tables on the GPU (compiler/tests/tables, plan step H.3): compiler/tests/table makes three
//! tables on the CPU (a sine, a product of waves, a ball's distance), uploads them, and reads each
//! at 4,096 points in a kernel. Each GPU read is the CPU's read of the same table at the same
//! point, within the two's rounding of its seven mixes (16 ulps of the larger of 1 and the value):
//! so within the table's proven error of the function, as the CPU's read is (the tables
//! package's G-sound tests).

use crate::built;
use wrela_host::{Host, Value};
use wrela_tests::f32s;

#[test]
#[ignore = "needs a GPU"]
fn a_tables_gpu_reads_are_its_cpu_reads() {
    let mut host = Host::load(built("table")).expect("load");
    host.call_export("on_gpu", &[]).expect("on_gpu");
    let made = host.buffers();
    let out = f32s(&host.read_buffer(*made.last().expect("buffers")).expect("read"));
    let errors = match host.call_export("errors", &[]).expect("errors").as_slice() {
        [Value::F32(a), Value::F32(b), Value::F32(c)] => [*a, *b, *c],
        other => panic!("errors returned {other:?}"),
    };
    let mut worst = [0.0f32; 3];
    for i in 0..4096usize {
        let cpu = match host.call_export("cpu", &[Value::I32(i as i32)]).expect("cpu").as_slice() {
            [Value::F32(a), Value::F32(b), Value::F32(c)] => [*a, *b, *c],
            other => panic!("cpu returned {other:?}"),
        };
        for t in 0..3 {
            let gpu = out[4 * i + t];
            let d = (gpu - cpu[t]).abs();
            worst[t] = worst[t].max(d);
            let room = 16.0 * f32::EPSILON * cpu[t].abs().max(1.0);
            assert!(
                d <= room,
                "table {}: point {i}: the GPU reads {gpu}, the CPU {}",
                t + 1,
                cpu[t]
            );
        }
    }
    eprintln!("errors {errors:?}; the GPU's reads from the CPU's at most {worst:?}");
}

/// Plan step H.1, a measurement (it asserts nothing of the shares): for each of the 25 corpus
/// fields (compiler/tests/fields' `proof`), over cells of 1/32, 1/64 and 1/128 of a 2 m box, the
/// share of 400 cells near the surface whose table would be proven within 1 mm. The gate was at
/// least 70% at 1/128 for at least half the fields; it failed (the pull request has the table).
///
///     cargo test --release -p wrela-tests --test suite -- table::h1_ --ignored --nocapture
#[test]
#[ignore = "a measurement: plan step H.1's table"]
fn h1_how_much_of_each_field_a_table_could_be_proven_to() {
    let build = wrela_host::CpuBuild::load(built("fields")).expect("load");
    let fields: Vec<u32> = (0..25).collect();
    let rows = wrela_tests::par_map(&fields, |&fi| {
        let mut host = build.start_with(1).expect("start");
        let mut row = Vec::new();
        for n in [32u32, 64, 128] {
            let args = [fi, n, 7, 400, 1].map(|x| Value::I32(x as i32));
            row.push(match host.call_export("proof", &args).expect("proof").as_slice() {
                [Value::F32(a), _, _] if *a < 0.0 => "does not derive".to_string(),
                [_, Value::F32(b), _] if *b == 0.0 => "no surface in the box".to_string(),
                [Value::F32(a), Value::F32(b), Value::F32(c)] => {
                    format!("{:.0}% of {b} ({c} by the second derivatives)", 100.0 * a / b)
                }
                other => format!("{other:?}"),
            });
        }
        (fi, row)
    });
    for (fi, row) in rows {
        eprintln!("| {} | {} |", crate::derive::NAMES[fi as usize], row.join(" | "));
    }
}
