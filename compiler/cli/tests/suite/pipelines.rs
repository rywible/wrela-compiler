//! `wrela pipelines` (§18.9): each site of a pipeline says whether its dispatch or draw runs
//! only under a runtime condition, by control dependence (`wrela_sema::mir::control`).

use crate::common;

const SHADERS: &str = r#"
@vertex
fn cover(v: std::gpu::VertexIndex) -> std::gpu::ClipPosition {
    std::gpu::ClipPosition { position: vec4(f32(v.index % 2) * 4.0 - 1.0, f32(v.index / 2) * 4.0 - 1.0, 0.0, 1.0) }
}

@fragment
fn colour() -> vec4 { vec4(1.0, 0.0, 0.0, 1.0) }
"#;

/// Whether each draw of a `frame` whose body is `body` is under a condition, in the order the
/// draws are written.
fn conditional(name: &str, body: &str) -> Vec<bool> {
    let main = format!(
        "pub fn frame(time: f32, width: u32, height: u32) {{\n    let screen = std::gpu::begin_screen_pass(clear: vec4())\n    let shown = time > 1.0\n{body}\n    screen.present()\n}}\n{SHADERS}"
    );
    let dir = common::package(
        &format!("pipelines-{name}"),
        &[("wrela.toml", &common::manifest("p")), ("main.wrela", &main)],
    );
    let v = common::json(common::wrela().arg("pipelines").arg(&dir).arg("--json"));
    let p = &v["pipelines"][0];
    p["conditional"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .map(|c| c.as_bool().unwrap())
        .collect()
}

const DRAW: &str = "std::gpu::draw(screen, cover, colour, vertices: 3)";

#[test]
fn a_draw_under_a_runtime_condition_is_marked() {
    for (name, body, want) in [
        ("arm", format!("    if shown {{\n        {DRAW}\n    }}"), true),
        (
            "joined",
            format!(
                "    var n = 0\n    if shown {{\n        n = 1\n    }} else {{\n        n = 2\n    }}\n    {DRAW}"
            ),
            false,
        ),
        (
            "guard",
            format!(
                "    if shown {{\n        screen.present()\n        return\n    }}\n    {DRAW}"
            ),
            true,
        ),
        ("literal", format!("    if true {{\n        {DRAW}\n    }}"), false),
        (
            "in-loop",
            format!(
                "    for i in 0..4 {{\n        if shown && i > 1 {{\n            {DRAW}\n        }}\n    }}"
            ),
            true,
        ),
        (
            "after-loop",
            format!(
                "    for i in 0..4 {{\n        if shown && i > 1 {{\n            let _x = i\n        }}\n    }}\n    {DRAW}"
            ),
            false,
        ),
        // A loop's own test is no condition; an early exit the code wrote is.
        ("loop-body", format!("    for i in 0..4 {{\n        {DRAW}\n    }}"), false),
        (
            "while-body",
            format!("    var i = 0\n    while i < 4 {{\n        {DRAW}\n        i += 1\n    }}"),
            false,
        ),
        (
            "after-break",
            format!(
                "    for i in 0..4 {{\n        if shown {{\n            break\n        }}\n        {DRAW}\n    }}"
            ),
            true,
        ),
    ] {
        assert_eq!(conditional(name, &body), [want], "{name}");
    }
}

/// The report for people says it too.
#[test]
fn the_report_says_which_sites_are_conditional() {
    let main = format!(
        "pub fn frame(time: f32, width: u32, height: u32) {{\n    let screen = std::gpu::begin_screen_pass(clear: vec4())\n    if time > 1.0 {{\n        {DRAW}\n    }}\n    screen.present()\n}}\n{SHADERS}"
    );
    let dir = common::package(
        "pipelines-text",
        &[("wrela.toml", &common::manifest("p")), ("main.wrela", &main)],
    );
    let out = common::wrela().arg("pipelines").arg(&dir).output().expect("run wrela");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && text.contains("(under a condition)"), "{text}");
}

/// A draw behind an associated constant that's `false` is no pipeline, however much code comes
/// before it in its function (§3.4): the constant is folded where it's read, so the branch on it
/// folds as one on a literal does. (An associated constant is a function, and the CPU's inliner
/// stops at its budget: a call it left past that kept the branch, and the pipeline behind it.)
#[test]
fn a_draw_behind_a_false_associated_constant_is_no_pipeline() {
    // A body past the inliner's budget before the draw, every value of it used (so none of it is
    // dropped as dead).
    let steps: String = (0..1500).map(|k| format!("    y = y * 1.5 + {k}.25\n")).collect();
    let main = format!(
        "trait Switches {{\n    const ON: bool = true\n}}\n\nstruct Off {{}}\n\nimpl Switches for Off {{\n    const ON: bool = false\n}}\n\npub fn frame(time: f32, width: u32, height: u32) {{\n    let screen = std::gpu::begin_screen_pass(clear: vec4())\n    var y = time\n{steps}    std::io::print(f\"{{y}}\")\n    if Off::ON {{\n        {DRAW}\n    }}\n    screen.present()\n}}\n{SHADERS}"
    );
    let dir = common::package(
        "pipelines-assoc",
        &[("wrela.toml", &common::manifest("p")), ("main.wrela", &main)],
    );
    let v = common::json(common::wrela().arg("pipelines").arg(&dir).arg("--json"));
    assert_eq!(v["pipelines"].as_array().map(Vec::len), Some(0), "{v}");
}
