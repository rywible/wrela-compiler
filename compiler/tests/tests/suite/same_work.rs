//! How often GPU code runs (language.md §18.13, compiler/ir/src/const_fold.rs and same.rs): a
//! value computed only from constants is computed as the shader is built, and a shipped build's
//! GPU code that computes a value the same for every invocation, at more than 256 work, is E0712.

use crate::{built, first_frame_buffers, scratch, write_package};

/// The folded cases (compiler/tests/folded) agree with the same math the GPU does, within its
/// error, and the folded kernel's shader computes none of it.
#[test]
fn folded_constants_are_what_the_gpu_computes() {
    let b = first_frame_buffers("folded", 2);
    let f32s = |b: &[u8]| -> Vec<f32> {
        b.chunks(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    };
    let (folded, computed) = (f32s(&b[0]), f32s(&b[1]));
    let src =
        std::fs::read_to_string(wrela_tests::repo_root().join("compiler/tests/folded/main.wrela"))
            .unwrap();
    let cases: u32 = src
        .lines()
        .find_map(|l| l.strip_prefix("pub const CASES: u32 = "))
        .and_then(|n| n.trim().parse().ok())
        .expect("CASES");
    for i in 0..cases as usize {
        let (a, b) = (folded[i], computed[i]);
        // WGSL's transcendentals are within an absolute 2^-11 or so of the true value.
        let close = (a - b).abs() <= 1.0e-3 + 1.0e-4 * b.abs();
        assert!(close, "case {i}: folded {a}, the GPU's {b}");
    }
    assert!(folded[..cases as usize].iter().any(|&x| x != 0.0));
    // The folded kernel's shader is constants: no math left in it.
    let dir = built("folded");
    let shader = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| std::fs::read_to_string(e.ok()?.path()).ok())
        .find(|s| s.contains("fn folded"))
        .expect("the folded kernel's shader");
    for f in ["sin(", "normalize(", "pow(", "smoothstep(", "exp2("] {
        assert!(!shader.contains(f), "the folded shader still calls {f}");
    }
}

/// A program of one kernel, `k`, given a uniform `u: f32` and computing `out[id]` with `body`
/// (which has `x`, the invocation's own `f32`), at workgroup size `size`.
fn kernel(name: &str, size: u32, body: &str) -> std::path::PathBuf {
    let dir = scratch(name);
    let src = format!(
        "use std::gpu::{{GlobalId, GpuBuffer, Slots, buffer, dispatch}}

struct Given: Copy + GpuData {{
    u: f32,
}}

@compute({size})
fn k(g: Given, out: mut Slots<f32>, id: GlobalId) {{
    let x = f32(id.x)
{body}
}}

pub fn frame(time: f32, width: u32, height: u32) {{
    var out: GpuBuffer<f32> = buffer(64)
    dispatch(k.bind(g: Given {{ u: time }}, out: mut out), over: 64)
}}
"
    );
    write_package(&dir, name, &src);
    dir
}

/// 40 rounds of `sin(a) + cos(a)` from `start`, written out, then `out[id]` from it and `x`.
fn chain(start: &str) -> String {
    let mut body = format!("    var a = {start}\n");
    for _ in 0..40 {
        body.push_str("    a = sin(a) + cos(a)\n");
    }
    body.push_str("    out[id] = a * x");
    body
}

/// A shipped build's E0712s (a check or a test's build reports none, as E0707).
fn e0712(dir: &std::path::Path) -> Vec<String> {
    let out = wrela_driver::build(dir);
    out.diagnostics
        .iter()
        .filter(|d| d.code == wrela_diag::codes::E0712)
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn work_every_invocation_redoes_alike_is_e0712() {
    let dir = kernel("same_uniform", 64, &chain("g.u"));
    let msgs = e0712(&dir);
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].contains("every invocation computes this again"), "{msgs:?}");
    // A check reports it as no error: only a shipped build is held to it.
    let checked = wrela_driver::check_pipelines(&dir);
    assert!(!checked.has_errors(), "{:?}", checked.diagnostics);
}

#[test]
fn what_varies_what_folds_and_one_invocation_kernels_are_not_e0712() {
    // From the invocation's own value: different in each.
    assert_eq!(e0712(&kernel("same_varying", 64, &chain("g.u + x"))), Vec::<String>::new());
    // From constants: computed once, as the shader is built.
    assert_eq!(e0712(&kernel("same_constant", 64, &chain("0.5"))), Vec::<String>::new());
    // A kernel of one invocation a workgroup: how GPU code computes something once.
    assert_eq!(e0712(&kernel("same_once", 1, &chain("g.u"))), Vec::<String>::new());
    // A little alike work is no error.
    let little = "    let a = sin(g.u) * 2.0\n    out[id] = a * x";
    assert_eq!(e0712(&kernel("same_little", 64, little)), Vec::<String>::new());
}

#[test]
fn a_loop_s_alike_work_counts_each_pass() {
    // 200 passes of a few work each: its whole work, not one pass's.
    let looped = "    var a = g.u\n    for _i in 0..200 {\n        a = sin(a) + cos(a)\n    }\n    out[id] = a * x";
    let msgs = e0712(&kernel("same_looped", 64, looped));
    assert_eq!(msgs.len(), 1, "{msgs:?}");
}

#[test]
fn small_alike_values_sharing_a_cheap_one_are_not_one_big_one() {
    // 60 terms of a few work each, all from one cheap same value: no term is much work, as
    // without the shared value.
    let mut shared = String::from("    let s = sin(g.u)\n    var acc = 0.0\n");
    let mut apart = String::from("    var acc = 0.0\n");
    for i in 0..60 {
        shared.push_str(&format!("    acc = acc + x * cos(s * {i}.5)\n"));
        apart.push_str(&format!("    acc = acc + x * cos(g.u * {i}.5)\n"));
    }
    shared.push_str("    out[id] = acc");
    apart.push_str("    out[id] = acc");
    assert_eq!(e0712(&kernel("same_shared", 64, &shared)), Vec::<String>::new());
    assert_eq!(e0712(&kernel("same_apart", 64, &apart)), Vec::<String>::new());
}

#[test]
fn constants_a_uniform_chooses_between_are_same_not_constant() {
    // `s` is only ever stored constants, but which one is the uniform's to say: the rounds
    // after are the same work in every invocation, and counted.
    let steered = format!(
        "    var s = 0.0\n    if g.u > 0.5 {{\n        s = 1.0\n        out[id] = 2.0\n    }}\n{}",
        chain("s")
    );
    assert_eq!(e0712(&kernel("same_steered", 64, &steered)).len(), 1);
}
