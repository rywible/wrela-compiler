//! Parse → format → parse gives the same AST, and formatting is idempotent, on hand-written
//! samples. (The grammar-generated round trips live in wrela-grammar's tests.)

use wrela_diag::FileId;
use wrela_syntax::{fmt, parse};

/// The AST's Debug output without spans and node ids.
fn shape(src: &str) -> String {
    let p = parse(FileId(0), src);
    assert!(!p.has_errors(), "parse errors in:\n{src}\n{:#?}", p.diagnostics);
    strip(&format!("{:#?}", p.file))
}

fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("Span {") {
        out.push_str(&rest[..i]);
        let j = rest[i..].find('}').map_or(rest.len(), |j| i + j + 1);
        out.push('S');
        rest = &rest[j..];
    }
    out.push_str(rest);
    out
}

fn round_trip(src: &str) -> String {
    let p = parse(FileId(0), src);
    assert!(!p.has_errors(), "parse errors:\n{:#?}", p.diagnostics);
    let once = fmt::format(&p, src);
    assert_eq!(shape(src), shape(&once), "format changed the AST:\n{once}");
    let p2 = parse(FileId(0), &once);
    let twice = fmt::format(&p2, &once);
    assert_eq!(once, twice, "formatting isn't idempotent");
    once
}

const SAMPLE: &str = r#"use std::field::{Surface, sphere, round_cone}
use std::gpu::{GlobalId, Slots}

/// A field.
pub fn blob(r: f32) -> SmoothUnion<Sphere, RoundCone> {
    sphere(radius: r)
        .smooth_union(round_cone(vec3(), vec3(y: 1.0), 0.3, 0.1), k: 0.1)
}

pub struct Grid: Copy + GpuData {
    origin: vec3, // where it starts
    cell: f32,
    n: u32 = 4,
}

enum Shape { Ball(f32), Box { half: vec3 }, Nothing }

trait Area {
    type Out
    fn area(self) -> f32
    fn twice(self) -> f32 { self.area() * 2.0 }
}

impl Area for Grid {
    type Out = f32

    fn area(self) -> f32 {
        self.cell * self.cell
    }
}

const K: f32 = 1.5
const N = 3

@compute(64)
fn sample<F: Surface>(field: F, grid: Grid, out: mut Slots<f32>, id: GlobalId) {
    let i = id.x
    let p = grid.origin + vec3(f32(i % grid.n), f32((i / grid.n) % grid.n), f32(i / (grid.n * grid.n))) * grid.cell
    out[i] = field.distance(p)
}

fn modes(a: take Grid, b: mut [f32], c: Option<Option<u32>>) -> borrow f32 {
    var x = take a; mut y = b[0]
    y += 1.0
    let (s, t) = (1, 2)
    for mut v in b { v = v * 2.0 ** -1.0 }
    for i in 0..10 { if i > 3 && !done { break } else if i == 2 { continue } else { x.cell = 1.0 } }
    while x.cell < 10.0 {
        x.cell *= 2.0
    }
    loop { return }
    let m = match c {
        Some(Some(n)) if n > 2 => n,
        Some(_) | None => 0,
    }
    let f = |q: vec3| -> f32 { q.x }
    let arr = [1, 2, 3]
    let rep = [0.0; 4]
    let t = arr.0
    let s = Shape::Box { half: vec3(1.0), ..base }
    f(take x, mut y, |a| a + 1, named: 2)
    mut b[0]
}
"#;

#[test]
fn sample_round_trips() {
    round_trip(SAMPLE);
}

#[test]
fn formatting_normalizes() {
    let out = round_trip("fn f(){let a=1;let b=a+2\n\n\n  let c = b}\n");
    assert_eq!(out, "fn f() {\n    let a = 1\n    let b = a + 2\n\n    let c = b\n}\n");
}

#[test]
fn comments_survive() {
    let src = "// top\n\n/// doc\nfn f() { // after brace\n    let a = 1 // trailing\n    // own line\n    let b = 2\n    // before close\n}\n// end\n";
    let out = round_trip(src);
    for c in [
        "// top",
        "/// doc",
        "// after brace",
        "// trailing",
        "// own line",
        "// before close",
        "// end",
    ] {
        assert!(out.contains(c), "lost {c}:\n{out}");
    }
}

#[test]
fn leading_dot_chains_are_kept() {
    let src = "fn f() {\n    let r = a\n        .b(1)\n        .c(2)\n}\n";
    assert_eq!(round_trip(src), src);
}

#[test]
fn multiline_arguments_are_kept() {
    let src = "fn f() {\n    g(\n        1,\n        2,\n    )\n}\n";
    assert_eq!(round_trip(src), src);
}

#[test]
#[ignore]
fn show_sample() {
    println!("{}", round_trip(SAMPLE));
}
