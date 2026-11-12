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
