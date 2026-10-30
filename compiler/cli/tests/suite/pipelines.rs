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

/// An associated constant of an `f64` comparison isn't folded (the folder takes floats as `f32`s,
/// where `16777217.0` and `16777216.0` are one number): its branch stays a run-time one, and is
/// the program's.
#[test]
fn an_f64_associated_constant_is_not_folded_as_an_f32() {
    let main = format!(
        "const A: f64 = 16777217.0\nconst B: f64 = 16777216.0\n\nstruct Off {{}}\n\nimpl Off {{\n    const SAME: bool = A == B\n}}\n\npub fn frame(time: f32, width: u32, height: u32) {{\n    let screen = std::gpu::begin_screen_pass(clear: vec4())\n    if Off::SAME {{\n        {DRAW}\n    }}\n    screen.present()\n}}\n{SHADERS}"
    );
    let dir = common::package(
        "pipelines-assoc-f64",
        &[("wrela.toml", &common::manifest("p")), ("main.wrela", &main)],
    );
    let v = common::json(common::wrela().arg("pipelines").arg(&dir).arg("--json"));
    assert_eq!(v["pipelines"][0]["conditional"], serde_json::json!([true]), "{v}");
}

/// A program's tier, an enum whose variants hold nothing, folds into the associated constants
/// its switches read (`!=` and `match`), so `--set TIER=Low` builds none of what `Low` turns
/// off; a `match` arm with a guard, or one that binds, leaves the constant a call.
#[test]
fn a_tier_folds_into_its_switches() {
    let main = |fog: &str| {
        format!(
            "pub enum Tier: Copy + Eq + Fieldless {{\n    Epic,\n    High,\n    Low,\n}}\n\npub const TIER: Tier = Tier::Epic\n\nstruct Mine {{}}\n\nimpl Mine {{\n    const FOG: bool = {fog}\n}}\n\npub fn frame(time: f32, width: u32, height: u32) {{\n    let screen = std::gpu::begin_screen_pass(clear: vec4())\n    if Mine::FOG {{\n        {DRAW}\n    }}\n    screen.present()\n}}\n{SHADERS}"
        )
    };
    let count = |name: &str, fog: &str, set: Option<&str>| {
        let main = main(fog);
        let dir =
            common::package(name, &[("wrela.toml", &common::manifest("p")), ("main.wrela", &main)]);
        let mut cmd = common::wrela();
        cmd.arg("pipelines").arg(&dir).arg("--json");
        if let Some(s) = set {
            cmd.arg("--set").arg(s);
        }
        let v = common::json(&mut cmd);
        v["pipelines"].as_array().expect("pipelines").len()
    };
    for (name, fog) in [
        ("tier-ne", "TIER != Tier::Low"),
        ("tier-match", "match TIER {\n        Tier::Low => false,\n        _ => true,\n    }"),
        (
            "tier-or",
            "match TIER {\n        Tier::High | Tier::Low => false,\n        Tier::Epic => true,\n    }",
        ),
    ] {
        assert_eq!(count(name, fog, None), 1, "{fog}: Epic draws");
        assert_eq!(count(name, fog, Some("TIER=Low")), 0, "{fog}: Low builds no fog");
    }
    // A guard isn't folded: the branch stays, and its pipeline with it.
    let guarded = "match TIER {\n        Tier::Low if true => false,\n        _ => true,\n    }";
    assert_eq!(count("tier-guard", guarded, Some("TIER=Low")), 1);
}

/// In a relaxed-SIMD build too (its presentation code is relaxable, and keeps strict code, such as
/// a constant `init` reads as well, a call), the branch on a `false` associated constant is gone,
/// and what only it reached.
#[test]
fn a_relaxed_build_folds_an_associated_constant() {
    let main = "trait Switches {\n    const ON: bool = true\n}\n\nstruct Off {}\n\nimpl Switches for Off {\n    const ON: bool = false\n}\n\npub struct S {\n    on: bool,\n}\n\npub fn init() -> S {\n    S { on: Off::ON }\n}\n\npub fn frame(s: mut S, time: f32, width: u32, height: u32) {\n    if Off::ON {\n        std::io::print(f\"behind the switch {time}\")\n    }\n}\n";
    let dir = common::package(
        "relaxed-assoc",
        &[("wrela.toml", &common::manifest("p")), ("main.wrela", main)],
    );
    let out = dir.join("out");
    let (code, text) = common::run(
        common::wrela().arg("build").arg(&dir).arg("-o").arg(&out).arg("--relaxed-simd"),
    );
    assert_eq!(code, Some(0), "{text}");
    // The branch's printing of a float is what makes a WASM big (230,147 bytes in 0.10.0's
    // relaxed build; its literal's bytes stay either way); without it, about 2 KB.
    for wasm in ["game.wasm", "game.relaxed.wasm"] {
        let size = std::fs::metadata(out.join(wasm)).expect(wasm).len();
        assert!(size < 20_000, "{wasm} keeps the branch: {size} bytes");
    }
}

/// A fragment shader may return its look's associated type (`Y::Out`), checked for each look it's
/// drawn with; and an `if` on an associated constant lowers only its taken branch, so a look's
/// pass of one target draws its one colour, another's of two its two, and neither instance's
/// other branch is a draw its pass can't take.
#[test]
fn a_fragment_returns_its_looks_type_in_the_pass_its_constant_picks() {
    let main = "use std::gpu::{ClipPosition, Pass, Pass2, R32Uint, RenderPass, Rgba16Float, Targets2, VertexIndex, begin_pass, begin_pass2, draw, texture}

pub trait Look {
    const MASK: u32 = 0
    type Out: Copy
    fn out(c: vec4, n: u32) -> Self::Out
}

pub struct Plain {}

impl Look for Plain {
    type Out = vec4
    fn out(c: vec4, n: u32) -> vec4 { c }
}

pub struct Full {}

impl Look for Full {
    const MASK: u32 = 1
    type Out = Targets2<vec4, u32>
    fn out(c: vec4, n: u32) -> Targets2<vec4, u32> { Targets2 { first: c, second: n } }
}

@vertex
fn tri(i: VertexIndex) -> ClipPosition {
    ClipPosition { position: vec4(f32(i.index % 2) * 4.0 - 1.0, f32(i.index / 2) * 4.0 - 1.0, 0.0, 1.0) }
}

@fragment
fn paint<Y: Look>(tint: f32) -> Y::Out {
    Y::out(vec4(tint), 7)
}

fn shade<P: RenderPass, Y: Look>(pass: P) {
    draw(pass, tri.bind(), paint::<Y>.bind(tint: 0.5), vertices: 3)
}

fn looked<Y: Look>() {
    var a = texture::<Rgba16Float>(width: 4, height: 4)
    var b = texture::<R32Uint>(width: 4, height: 4)
    if Y::MASK == 0 {
        let p = begin_pass(mut a)
        shade::<Pass<Rgba16Float>, Y>(p)
        p.end()
    } else {
        let p = begin_pass2(mut a, mut b)
        shade::<Pass2<Rgba16Float, R32Uint>, Y>(p)
        p.end()
    }
}

pub fn frame(time: f32, width: u32, height: u32) {
    looked::<Plain>()
    looked::<Full>()
}
";
    let dir = common::package(
        "pipelines-look-out",
        &[("wrela.toml", &common::manifest("p")), ("main.wrela", main)],
    );
    let v = common::json(common::wrela().arg("pipelines").arg(&dir).arg("--json"));
    let pipelines = v["pipelines"].as_array().expect("pipelines");
    assert_eq!(pipelines.len(), 2, "{v}");
    let mut bytes: Vec<u64> =
        pipelines.iter().filter_map(|p| p["attachment_bytes"].as_u64()).collect();
    bytes.sort_unstable();
    assert_eq!(bytes, vec![8, 12], "{v}");
}

/// `--set LOOK=Full` points the program's `pub type LOOK` at another type, so the program builds
/// in that look (its shading's two targets in place of one); an alias that isn't `pub` can't be
/// set (E0224).
#[test]
fn a_set_points_a_programs_look_at_another_type() {
    let main = |vis: &str| {
        format!("use std::gpu::{{ClipPosition, Pass, Pass2, R32Uint, RenderPass, Rgba16Float, Targets2, VertexIndex, begin_pass, begin_pass2, draw, texture}}

pub trait Look {{
    const MASK: u32 = 0
    type Out: Copy
    fn out(c: vec4, n: u32) -> Self::Out
}}

pub struct Plain {{}}

impl Look for Plain {{
    type Out = vec4
    fn out(c: vec4, n: u32) -> vec4 {{ c }}
}}

pub struct Full {{}}

impl Look for Full {{
    const MASK: u32 = 1
    type Out = Targets2<vec4, u32>
    fn out(c: vec4, n: u32) -> Targets2<vec4, u32> {{ Targets2 {{ first: c, second: n }} }}
}}

{vis}type LOOK = Plain

@vertex
fn tri(i: VertexIndex) -> ClipPosition {{
    ClipPosition {{ position: vec4(f32(i.index % 2) * 4.0 - 1.0, f32(i.index / 2) * 4.0 - 1.0, 0.0, 1.0) }}
}}

@fragment
fn paint<Y: Look>(tint: f32) -> Y::Out {{
    Y::out(vec4(tint), 7)
}}

pub fn frame(time: f32, width: u32, height: u32) {{
    var a = texture::<Rgba16Float>(width: 4, height: 4)
    var b = texture::<R32Uint>(width: 4, height: 4)
    if LOOK::MASK == 0 {{
        let p = begin_pass(mut a)
        draw(p, tri.bind(), paint::<LOOK>.bind(tint: 0.5), vertices: 3)
        p.end()
    }} else {{
        let p = begin_pass2(mut a, mut b)
        draw(p, tri.bind(), paint::<LOOK>.bind(tint: 0.5), vertices: 3)
        p.end()
    }}
}}
")
    };
    let bytes = |name: &str, vis: &str, set: Option<&str>| {
        let main = main(vis);
        let dir =
            common::package(name, &[("wrela.toml", &common::manifest("p")), ("main.wrela", &main)]);
        let mut cmd = common::wrela();
        cmd.arg("pipelines").arg(&dir).arg("--json");
        if let Some(s) = set {
            cmd.arg("--set").arg(s);
        }
        common::json(&mut cmd)
    };
    let plain = bytes("look-plain", "pub ", None);
    assert_eq!(plain["pipelines"][0]["attachment_bytes"], serde_json::json!(8), "{plain}");
    let full = bytes("look-full", "pub ", Some("LOOK=Full"));
    assert_eq!(full["pipelines"].as_array().map(Vec::len), Some(1), "{full}");
    assert_eq!(full["pipelines"][0]["attachment_bytes"], serde_json::json!(12), "{full}");
    let private = bytes("look-private", "", Some("LOOK=Full"));
    assert!(private.to_string().contains("E0224"), "{private}");
}

/// An associated constant made of others with `|`, `&`, `^` and shifts folds (a look's mask,
/// `FORM | TONE`): a branch it rules out builds no pipeline, and one it doesn't builds its own.
#[test]
fn a_mask_made_with_bitwise_operators_folds() {
    let main = format!(
        "const FORM: u32 = 1\nconst CAUSE: u32 = 2\nconst TONE: u32 = 4\n\ntrait Look {{\n    const MASK: u32 = 0\n}}\n\nstruct Cel {{}}\n\nimpl Look for Cel {{\n    const MASK: u32 = FORM | TONE ^ (CAUSE << 1) | (8 >> 3) & 1\n}}\n\npub fn frame(time: f32, width: u32, height: u32) {{\n    let screen = std::gpu::begin_screen_pass(clear: vec4())\n    if (Cel::MASK & CAUSE) != 0 {{\n        {DRAW}\n    }}\n    screen.present()\n}}\n{SHADERS}"
    );
    let dir = common::package(
        "pipelines-assoc-mask",
        &[("wrela.toml", &common::manifest("p")), ("main.wrela", &main)],
    );
    let v = common::json(common::wrela().arg("pipelines").arg(&dir).arg("--json"));
    assert_eq!(v["pipelines"].as_array().map(Vec::len), Some(0), "{v}");
    let on = main.replace("(Cel::MASK & CAUSE) != 0", "(Cel::MASK & FORM) != 0");
    let dir = common::package(
        "pipelines-assoc-mask-on",
        &[("wrela.toml", &common::manifest("p")), ("main.wrela", &on)],
    );
    let v = common::json(common::wrela().arg("pipelines").arg(&dir).arg("--json"));
    assert_eq!(v["pipelines"].as_array().map(Vec::len), Some(1), "{v}");
}
