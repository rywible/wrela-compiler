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

#[test]
fn a_loop_s_counter_is_its_structure_unless_a_uniform_bounds_or_indexes_it() {
    // Directions from a constant loop's counters, divisions and all: computed from nothing the
    // command gives, so nothing to pass in.
    let counted = "    var acc = 0.0\n    for i in 0..8 {\n        for j in 0..8 {\n            let ct = 1.0 - 2.0 * (f32(i) + 0.5) / 8.0\n            let phi = 6.2831853 * (f32(j) + 0.5) / 8.0\n            let st = sqrt(max(1.0 - ct * ct, 0.0))\n            acc += dot(vec3(st * cos(phi), ct, st * sin(phi)), vec3(x, 1.0, 0.5))\n        }\n    }\n    out[id] = acc";
    assert_eq!(e0712(&kernel("same_counted", 64, counted)), Vec::<String>::new());
    // A counter the uniform bounds: what it reaches is the uniform's to say.
    let bounded = format!(
        "    var s = 0.0\n    var i = 0\n    while i < i32(g.u) {{\n        s = s + 1.0\n        i = i + 1\n    }}\n{}",
        chain("s")
    );
    assert_eq!(e0712(&kernel("same_bounded", 64, &bounded)).len(), 1);
    // An element the uniform picks.
    let indexed =
        format!("    var t = [0.0, 0.0, 0.0, 0.0]\n    t[i32(g.u) % 4] = 1.0\n{}", chain("t[0]"));
    assert_eq!(e0712(&kernel("same_indexed", 64, &indexed)).len(), 1);
}

/// A kernel whose invocations call `sky` (40 rounds of math) with a direction read from a
/// buffer, and with one computed from the invocation's own index; and `cheap` with the buffer's.
fn few_kernel(name: &str) -> std::path::PathBuf {
    let dir = scratch(name);
    let src = "use std::gpu::{GlobalId, GpuBuffer, LocalId, Shared, Slots, barrier, buffer, dispatch}

struct Given: Copy + GpuData {
    u: f32,
}

fn sky(d: vec3, u: f32) -> f32 {
    var a = d.x + u
    for _i in 0..40 {
        a = sin(a) + cos(a * d.y + d.z)
    }
    a
}

fn cheap(d: vec3) -> f32 {
    d.x * 2.0 + d.y
}

// Six numbers vary: looked into.
fn shade(p: vec3, n: vec3, u: f32) -> f32 {
    sky(n, u) + p.x
}

// Lent a place, returning nothing: looked into.
fn add_sky(acc: mut f32, d: vec3, u: f32) {
    acc += sky(d, u)
}

// Bounded by what it's given: cheap at two passes.
fn bumps(d: vec3, n: u32) -> f32 {
    var a = 0.0
    for i in 0..n {
        a += d.x * f32(i)
    }
    a
}

@compute(64)
fn k(g: Given, dirs: [vec4], out: mut Slots<f32>, id: GlobalId) {
    let x = f32(id.x)
    let d = dirs[id.x].xyz
    let q = dirs[id.x + 1].xyz
    var acc = 0.0
    for i in 0..3 {
        acc = acc + sky(d * f32(i + 1), g.u)
    }
    add_sky(mut acc, q, g.u)
    let a = shade(q, d, g.u)
    let b = shade(d, q, g.u)
    out[id] = acc + a + b + sky(vec3(x, x * 2.0, 1.0), g.u) + cheap(d) + bumps(d, 2)
}

@compute(64)
fn wide(g: Given, dirs: [vec4], out: mut Slots<f32>, shared: mut Shared<f32, 64>, id: GlobalId, lid: LocalId) {
    let d = dirs[id.x].xyz
    let c = shared.chunk(lid)
    shared.write(c, 0, dirs[id.x].w)
    barrier(mut shared)
    // (The workgroup's first: it differs from workgroup to workgroup.)
    let w = shared.get(0)
    out[id] = sky(d.xy.xxy, w)
}

pub fn frame(time: f32, width: u32, height: u32) {
    let dirs: GpuBuffer<vec4> = buffer(64)
    var out: GpuBuffer<f32> = buffer(64)
    dispatch(k.bind(Given { u: time }, dirs, out: mut out), over: 64)
    dispatch(wide.bind(Given { u: time }, dirs, out: mut out), groups: 1)
}
";
    write_package(&dir, name, src);
    dir
}

/// `wrela pipelines` lists a costly function called with a few numbers that vary, all else the
/// same, with each of its runs counted (a loop's passes, each call of a function it's called
/// through): what a table could replace. A call given only the invocation's own place isn't one
/// (a table of it is as large as the dispatch), nor is a cheap one, nor one given what an
/// invocation writes.
#[test]
fn calls_with_few_varying_numbers_are_listed_for_tables() {
    let dir = few_kernel("few_inputs");
    let out = wrela_driver::check_pipelines(&dir);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let p = out.pipelines.iter().find(|p| p.names[0] == "k").expect("k's pipeline");
    let few = &p.few[0];
    assert_eq!(few.len(), 1, "{few:?}");
    assert_eq!(few[0].function, "sky");
    assert_eq!(few[0].varying, 3);
    // Each pass of the loop, the call through `add_sky` and each through `shade`; not the call
    // at the invocation's own place, which would make seven.
    assert_eq!(few[0].calls, 6, "{few:?}");
    assert!(few[0].work.is_some_and(|w| w % 6 == 0 && w >= 6 * 40 * 4), "{few:?}");
    assert!(few[0].at.contains("main.wrela"), "{few:?}");
    // Workgroup memory differs between workgroups: given it, `sky` takes four varying numbers.
    let wide = out.pipelines.iter().find(|p| p.names[0] == "wide").expect("wide's pipeline");
    assert!(wide.few[0].is_empty(), "{:?}", wide.few);
    // A build doesn't look for them.
    let built = wrela_driver::check(&dir);
    assert!(built.pipelines.iter().all(|p| p.few.iter().all(|c| c.is_empty())));
}
