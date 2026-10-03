//! GPU programs run by the native host: a draw whose shaders read buffers (bound in the order
//! the pipeline declares them), `len()` of a storage buffer, and what the WGSL back end must
//! take care with (compiler/tests/shaders). Need a GPU:
//! `cargo test -p wrela-tests --test suite render:: -- --ignored`.

use crate::built;
use wrela_host::{Host, Value};

#[test]
#[ignore = "needs a GPU"]
fn shaders_read_buffers() {
    let run = Host::load(built("render")).expect("load").run_frames(&[0.0], 16, 4).expect("run");
    let px = |x: usize, y: usize| &run.frame[4 * (y * 16 + x)..4 * (y * 16 + x) + 4];
    // Half of red on the left, half of blue on the right (rgba8unorm rounds 127.5 up).
    assert_eq!(px(2, 1), [128, 0, 0, 128]);
    assert_eq!(px(12, 2), [0, 0, 128, 128]);
}

#[test]
#[ignore = "needs a GPU"]
fn lengths_of_runs_and_buffers() {
    let mut host = Host::load(built("lengths")).expect("load");
    // On the CPU: a run's length, and an array's.
    match host.call_export("lens", &[]).expect("lens").as_slice() {
        [Value::F32(sum), Value::F32(len), _] => assert_eq!((*sum, *len), (10.0, 4.0)),
        other => panic!("lens returned {other:?}"),
    }
    // On the GPU: a buffer of 10 elements has length 10, whatever the host rounds its size to.
    host.run_frames(&[0.0], 4, 4).expect("run");
    let counts = wrela_tests::u32s(&host.read_buffer(1).expect("out"));
    assert_eq!(&counts[..4], &[10, 11, 12, 13]);
}

/// Without a GPU too: the WGSL back end takes every shader (naga validates it, and parses the
/// WGSL it writes).
#[test]
fn shaders_build() {
    built("shaders");
}

/// GPU code at the edges of what the back ends lower: each builds (its WGSL valid), rather
/// than failing with an internal error. Without a GPU.
#[test]
fn gpu_code_at_its_edges_builds() {
    let kernels = [
        // A tuple none of whose elements has a value: no empty WGSL struct.
        ("unit_tuple", "", "let t = (2.0, ((), ()))\n    out[id] = t.0"),
        // A matrix element, in a pipeline that makes no `vec2` of its own.
        ("matrix_element", "", "out[id] = u.m[1][1]"),
        // A helper that never returns: no result to take from it.
        (
            "endless_helper",
            "fn spin(x: f32) -> f32 {\n    loop {\n    }\n}\n",
            "if id.x > 100000 {\n        out[id] = spin(1.0)\n    }",
        ),
        // Constant indexes past the end: they trap on the CPU, and WGSL rejects them as
        // constants, so the GPU reads them at run time (robust access).
        (
            "constant_index_outside",
            "const K: u32 = 5\n",
            "let a = [1.0, 2.0, 3.0]\n    let v = vec3(1.0, 2.0, 3.0)\n    \
             if id.x > 100000 {\n        out[id] = a[5] + v[K] + u.m[K].x + a[1]\n    }",
        ),
    ];
    for (name, items, body) in kernels {
        let src = format!(
            "use std::gpu::{{GlobalId, GpuBuffer, Slots, buffer, dispatch}}

struct U: Copy + Clone + GpuData {{
    m: mat2,
}}

{items}

@compute(64)
fn k(u: U, out: mut Slots<f32>, id: GlobalId) {{
    {body}
}}

pub fn frame(time: f32, width: u32, height: u32) {{
    let out: GpuBuffer<f32> = buffer(64)
    let u = U {{ m: mat2(vec2(1.0, 2.0), vec2(3.0, 4.0)) }}
    dispatch(k, groups: 1, u: u, out: out)
}}
"
        );
        if let Err(e) = wrela_tests::build(&crate::package(&format!("edges/{name}"), &src)) {
            panic!("{name} doesn't build:\n{e}");
        }
    }
}

#[test]
fn many_early_returns_nest_little() {
    // 130 returns in a row, and a value from before them used after them: once inlined, each
    // return puts what follows under one more `if`, but WGSL allows 127 levels of braces.
    let mut pick = String::from("fn pick(x: f32) -> f32 {\n    let base = x * 2.0\n");
    for k in 0..130 {
        pick += &format!("    if x < {k}.5 {{\n        return {k}.0\n    }}\n");
    }
    pick += "    base\n}\n";
    let src = format!(
        "use std::gpu::{{GlobalId, GpuBuffer, Slots, buffer, dispatch}}

{pick}
@compute(64)
fn k(out: mut Slots<f32>, id: GlobalId) {{
    out[id] = pick(f32(id.x))
}}

pub fn frame(time: f32, width: u32, height: u32) {{
    let out: GpuBuffer<f32> = buffer(64)
    dispatch(k, groups: 1, out: out)
}}
"
    );
    let built = wrela_tests::build(&crate::package("early_returns", &src))
        .unwrap_or_else(|e| panic!("doesn't build:\n{e}"));
    let mut shaders = 0;
    for (path, bytes) in &built.files {
        if !path.ends_with(".wgsl") {
            continue;
        }
        shaders += 1;
        let (mut depth, mut deepest) = (0i32, 0);
        for c in String::from_utf8_lossy(bytes).chars() {
            match c {
                '{' => {
                    depth += 1;
                    deepest = deepest.max(depth);
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        assert!(deepest <= 32, "{path} nests {deepest} deep");
    }
    assert_eq!(shaders, 1);
}

#[test]
#[ignore = "needs a GPU"]
fn shaders_get_the_values_the_program_means() {
    let mut host = Host::load(built("shaders")).expect("load");
    let run = host.run_frames(&[0.0], 16, 4).expect("run");
    let px = |x: usize, y: usize| &run.frame[4 * (y * 16 + x)..4 * (y * 16 + x) + 4];
    // A vertex output of only a `ClipPosition` covers the screen; the generic pair, drawn with
    // `f32` then `vec4`, the bottom left and bottom right.
    assert_eq!(px(3, 0), [0, 0, 255, 255]);
    assert_eq!(px(3, 3), [255, 0, 0, 255]);
    assert_eq!(px(12, 3), [0, 255, 0, 255]);
    let got = wrela_tests::f32s(&host.read_buffer(0).expect("out"));
    // Integer `/ 0` gives the dividend, a shift takes its amount modulo 32, and `1.0 / 0.0` and
    // `3e38 * 10.0` are infinite; then `^` of bools, `-` of a matrix, `select` of enums, an enum
    // in a struct, a uniform after one with no value, the last row of a 80 KB table, a buffer's
    // length through a closure, and a second `GlobalId`.
    let want = [7.0, 2.0, 5.0, 6.0, 1.0, -5.0, 6.0, 7.5, 3.0, 9.0, 3.0, 11.0];
    assert_eq!(&got[..want.len()], &want);
}
