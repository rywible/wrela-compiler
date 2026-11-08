//! Work and weight (language.md §18.11, compiler/ir/src/work.rs): `wrela pipelines` counts each
//! entry point's most work in one call and each pipeline's weight, the same on every machine;
//! `@budget(work: n)` and `@budget(weight: n)` hold a GPU function to them (E0710).

use crate::{scratch, write_package};

/// A program of one kernel per body, each dispatched: `kernels` are `(name, body)`, the body
/// computing `out[id]` from `x`, an `f32` the invocation has.
fn kernels(name: &str, items: &str, kernels: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = scratch(name);
    let mut src = String::from("use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}\n\n");
    src.push_str(items);
    for (k, body) in kernels {
        src.push_str(&format!(
            "\n@compute(64)\nfn {k}(out: mut Slots<f32>, id: GlobalId) {{\n    let x = f32(id.x)\n{body}\n}}\n"
        ));
    }
    src.push_str("\npub fn frame(time: f32, width: u32, height: u32) {\n    var out: GpuBuffer<f32> = buffer(64)\n");
    for (k, _) in kernels {
        src.push_str(&format!("    dispatch({k}.bind(mut out), over: 64)\n"));
    }
    src.push_str("}\n");
    write_package(&dir, name, &src);
    dir
}

/// Each kernel's work, by name.
fn work(dir: &std::path::Path) -> Vec<(String, Option<u64>, u64)> {
    let out = wrela_driver::check_pipelines(dir);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    out.pipelines
        .iter()
        .map(|p| (p.names[0].clone(), p.work.first().copied().flatten(), p.weight))
        .collect()
}

fn of(all: &[(String, Option<u64>, u64)], k: &str) -> (Option<u64>, u64) {
    let (_, w, g) = all.iter().find(|(n, ..)| n == k).unwrap_or_else(|| panic!("{k}: {all:?}"));
    (*w, *g)
}

#[test]
fn loops_count_each_pass_and_unbounded_ones_have_no_work() {
    let dir = kernels(
        "work_loops",
        "",
        &[
            (
                "eight",
                "    var a = 0.0\n    for _i in 0..8 {\n        a = a + sin(x + f32(_i))\n    }\n    out[id] = a",
            ),
            (
                "sixteen",
                "    var a = 0.0\n    for _i in 0..16 {\n        a = a + sin(x + f32(_i))\n    }\n    out[id] = a",
            ),
            // Left early on the invocation's own value, but never past 32 passes.
            (
                "capped",
                "    var a = 0.0\n    var i = 0\n    var done = false\n    while i < 32 && !done {\n        a = a + sin(x + a)\n        done = a > 10.0\n        i = i + 1\n    }\n    out[id] = a",
            ),
            // A bound the loop itself moves by the invocation's value: what the first pass knew
            // of it isn't known on the next.
            (
                "moved",
                "    var i: u32 = 0\n    var n: u32 = 4\n    while i < n {\n        i = i + 1\n        n = n + id.x\n    }\n    out[id] = f32(i)",
            ),
            // Left only on the invocation's own value: no sure exit.
            (
                "open",
                "    var a = 0.0\n    while a < 10.0 {\n        a = a + sin(x + a) + 0.5\n    }\n    out[id] = a",
            ),
        ],
    );
    let all = work(&dir);
    let (eight, sixteen) =
        (of(&all, "eight").0.expect("bounded"), of(&all, "sixteen").0.expect("bounded"));
    let per_pass = sixteen - eight;
    assert!(per_pass >= 8 * 4, "eight more passes, each a sine (4) and more: {all:?}");
    assert!(eight < sixteen && sixteen < 2 * eight + 16, "{all:?}");
    let capped = of(&all, "capped").0.expect("a counter that reaches 32 bounds it");
    assert!(capped >= 32 * 4, "{all:?}");
    assert_eq!(of(&all, "open").0, None, "{all:?}");
    assert_eq!(of(&all, "moved").0, None, "{all:?}");
}

#[test]
fn a_branch_counts_its_costlier_side_unless_its_condition_is_known() {
    let items = "fn heavy(x: f32) -> f32 {\n    var a = x\n    for _i in 0..32 {\n        a = sin(a) + cos(a)\n    }\n    a\n}\n";
    let dir = kernels(
        "work_branches",
        items,
        &[
            ("either", "    out[id] = if x > 3.0 { heavy(x) } else { x + 1.0 }"),
            ("never", "    out[id] = if false { heavy(x) } else { x + 1.0 }"),
            ("plain", "    out[id] = heavy(x)"),
        ],
    );
    let all = work(&dir);
    let (either, plain, never) =
        (of(&all, "either").0.unwrap(), of(&all, "plain").0.unwrap(), of(&all, "never").0.unwrap());
    assert!(either >= plain, "the costlier side counts: {all:?}");
    assert!(never * 10 < plain, "a known branch counts the side it takes: {all:?}");
}

#[test]
fn a_function_called_from_more_places_weighs_more() {
    let items = "fn big(x: f32) -> f32 {\n    var a = x\n    for _i in 0..4 {\n        a = sin(a) * cos(a) + sqrt(abs(a)) + exp(a * 0.1)\n    }\n    a\n}\n";
    let dir = kernels(
        "work_weight",
        items,
        &[
            (
                "once",
                "    var a = x\n    for _i in 0..4 {\n        a = big(a)\n    }\n    out[id] = a",
            ),
            ("four", "    out[id] = big(big(big(big(x))))"),
        ],
    );
    let all = work(&dir);
    let (once, four) = (of(&all, "once"), of(&all, "four"));
    let (a, b) = (once.0.unwrap(), four.0.unwrap());
    assert!(
        a.abs_diff(b) * 10 < a,
        "about the same work either way (the loop's counter aside): {all:?}"
    );
    assert!(
        four.1 * 10 > once.1 * 18,
        "four calls weigh four copies, a loop of one call one: {all:?}"
    );
}

#[test]
fn a_function_over_its_budget_is_e0710() {
    let items = "@budget(work: 50)\nfn heavy(x: f32) -> f32 {\n    var a = x\n    for _i in 0..32 {\n        a = sin(a) + cos(a)\n    }\n    a\n}\n\n@budget(weight: 10)\nfn wide(x: f32) -> f32 {\n    heavy(heavy(x))\n}\n\n@budget(work: 1000)\nfn open(x: f32) -> f32 {\n    var a = x\n    while a < 10.0 {\n        a = a + 1.0\n    }\n    a\n}\n";
    let dir = kernels("work_budget", items, &[("k", "    out[id] = wide(x) + open(x)")]);
    let out = wrela_driver::check_pipelines(&dir);
    let msgs: Vec<String> = out
        .diagnostics
        .iter()
        .filter(|d| d.code == wrela_diag::codes::E0710)
        .map(|d| d.message.clone())
        .collect();
    assert_eq!(msgs.len(), 3, "{:?}", out.diagnostics);
    assert!(
        msgs.iter().any(|m| m.contains("`heavy` can do") && m.contains("over its budget of 50")),
        "{msgs:?}"
    );
    assert!(msgs.iter().any(|m| m.contains("`wide` weighs")), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("`open`'s work has no bound")), "{msgs:?}");
}
