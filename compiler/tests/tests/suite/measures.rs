//! Measures and specs (language.md §23): std::field's measures find a surface where it is, to
//! the precision of their Newton step, and std::lift's `Report` and `Loss` say how a spec's
//! checks stand. Run as `@test` functions of a package (`wrela test`).

use crate::package;

const TESTS: &str = "use std::derive::Box3
use std::field::{
    Surface, back, bottom, farthest, front, hit, oval, part_named, sphere, top, width,
}
use std::lift::{Checks, Loss, Report}

fn close(a: f32, b: f32) -> bool {
    abs(a - b) < 0.0002
}

/// A ball of radius 0.5 at (0.1, 0.3, -0.2).
fn ball() -> Surface {
    sphere(0.5).translate(vec3(0.1, 0.3, -0.2))
}

@test
fn rays_meet_the_surface() {
    let f = ball()
    assert(close(hit(f, vec3(0.1, 2.0, -0.2), vec3(0.0, -1.0, 0.0)), 1.2))
    assert(hit(f, vec3(3.0, 2.0, 0.0), vec3(0.0, -1.0, 0.0)) == -1.0)
    assert(hit(f, vec3(0.1, 2.0, -0.2), vec3(0.0, -1.0, 0.0), far: 1.0) == -1.0)
}

@test
fn heights_widths_and_depths() {
    let f = ball()
    assert(close(top(f, x: 0.1, z: -0.2), 0.8))
    assert(close(bottom(f, x: 0.1, z: -0.2), -0.2))
    assert(close(width(f, y: 0.3, z: -0.2), 1.0))
    assert(close(front(f, x: 0.1, y: 0.3), 0.3))
    assert(close(back(f, x: 0.1, y: 0.3), -0.7))
    // Off the surface: the stated defaults.
    assert(top(f, x: 2.0, z: 0.0) == 0.0)
    assert(width(f, y: 2.0, z: 0.0) == 0.0)
    assert(front(f, x: 2.0, y: 0.0) == -10.0)
}

@test
fn farthest_finds_the_reach_within_a_region() {
    let f = ball()
    let all = Box3 { lo: vec3(-2.0), hi: vec3(2.0) }
    assert(abs(farthest(f, vec3(1.0, 0.0, 0.0), all) - 0.6) < 0.001)
    // Only the ball's lower half: its reach up is its centre's height.
    let low = Box3 { lo: vec3(-2.0), hi: vec3(2.0, 0.3, 2.0) }
    assert(abs(farthest(f, vec3(0.0, 1.0, 0.0), low) - 0.3) < 0.001)
}

@test
fn an_oval_has_its_radii() {
    let f = oval(vec3(0.3, 0.2, 0.6))
    assert(close(top(f, x: 0.0, z: 0.0), 0.2))
    assert(close(width(f, y: 0.0, z: 0.0), 0.6))
    assert(close(front(f, x: 0.0, y: 0.0), 0.6))
}

@test
fn parts_are_found_by_name() {
    let f = sphere(0.2).named(\"head\").union(sphere(0.5).translate(vec3(1.0)).named(\"body\"))
    assert(part_named(f, \"body\") == 1)
    assert(part_named(f, \"tail\") == f.parts())
}

fn spec<C: Checks>(c: mut C) {
    let f = ball()
    c.near(\"height\", top(f, x: 0.1, z: -0.2), 0.8, within: 0.01)
    c.near(\"width\", width(f, y: 0.3, z: -0.2), 0.9, within: 0.01)
    c.at_least(\"depth\", front(f, x: 0.1, y: 0.3) - back(f, x: 0.1, y: 0.3), 0.9)
    c.at_most(\"underside\", bottom(f, x: 0.1, z: -0.2), -0.3, within: 0.05)
}

@test
fn a_report_says_which_checks_hold() {
    var r = Report::new()
    spec(mut r)
    assert(r.held == 2 && r.missed == 2)
    let json = r.done()
    assert(json.as_str().starts_with(\"{\\\"checks\\\":[{\\\"name\\\":\\\"height\\\",\\\"check\\\":\\\"near\\\"\"))
}

@test
fn a_loss_sums_the_misses_over_their_allowances() {
    var l = Loss::new()
    spec(mut l)
    // width: 0.1 / 0.01 = 10; underside: 0.1 / 0.05 = 2; the others hold.
    assert(abs(l.total - 104.0) < 0.1)
}
";

#[test]
fn measures_and_specs() {
    assert_eq!(super::tests_pass(&package("measures", TESTS)), 7);
}

/// A derivative through an antialiased edge (stage D's gate): the sum of a sphere's coverage
/// over a 32 × 32 grid, derived by its radius (a lifted literal) with `std::lift::gradient`,
/// agrees within 10% with the central difference (radius ± 0.002) of the same sum with a hard
/// edge, 16 × 16 points a cell.
#[test]
fn coverage_has_the_edges_derivative() {
    use wrela_host::{CpuBuild, Value};
    let dir = crate::built_lifted("outline", &["outline"]);
    let mut host = CpuBuild::load(dir).expect("load").start().expect("start");
    let slope = f64::from(wrela_tests::one_f32(&mut host, "soft_slope", &[]));
    let (r, h) = (0.437f32, 0.002f32);
    let up = f64::from(wrela_tests::one_f32(&mut host, "hard_sum", &[Value::F32(r + h)]));
    let down = f64::from(wrela_tests::one_f32(&mut host, "hard_sum", &[Value::F32(r - h)]));
    let central = (up - down) / f64::from(2.0 * h);
    let soft = f64::from(wrela_tests::one_f32(&mut host, "soft_sum", &[]));
    let hard = f64::from(wrela_tests::one_f32(&mut host, "hard_sum", &[Value::F32(r)]));
    println!(
        "coverage over 32 × 32 cells: derived slope {slope:.2}, the hard edge's central \
         difference {central:.2} ({:.1}% apart); sums {soft:.2} and {hard:.2}",
        100.0 * (slope - central).abs() / central.abs()
    );
    assert!((slope - central).abs() <= 0.1 * central.abs(), "{slope} against {central}");
}

/// A spec whose check is an `outline_miss` against a blueprint's outline (a `Vec`, §23.7),
/// its loss derived by `std::lift::gradient` handed the loss through a parameter, as `wrela
/// solve` hands it: it builds with no E0700 (the outline is read by a function given nothing
/// the derivation follows, §14.2), and the loss's derivative by the sphere's radius isn't 0.
/// Only the subject's file is lifted, as `wrela solve` lifts only the files it frees: the
/// blueprint's own numbers are a reference, not something to solve for.
#[test]
fn an_outline_from_a_blueprints_vec_leaves_the_loss_derivable() {
    let blueprint = "use std::collections::Vec

/// A side: a 32-gon of radius 0.5.
pub fn side() -> Vec<vec2> {
    Vec::from_fn(32, |i| {
        let a = 6.2831853 * f32(i) / 32.0
        vec2(0.5 * cos(a), 0.5 * sin(a))
    })
}
";
    let src = "use blueprint::side
use std::field::{View, outline_miss, sphere}
use std::lift::{Checks, Literal, Loss, count, gradient, literal}

fn spec<C: Checks>(c: mut C) {
    let f = sphere(0.4321)
    c.at_most(\"side outline\", outline_miss(f, View::Side, side(), 0.04, near: -1.0, far: 1.0), 0.0, within: 0.05)
}

fn loss() -> f32 {
    var l = Loss::new()
    spec(mut l)
    l.total
}

/// The loss's derivative by literal `k`, the gradient handed `f` through a parameter.
fn slope(f: fn() -> f32, k: u32) -> f32 {
    let (_, d) = gradient(f, [Literal { index: k }])
    d[0]
}

/// The radius's literal: the first whose value is 0.4321.
fn radius() -> u32 {
    var k = count()
    for i in 0..count() {
        if let Some(l) = literal(i) {
            if l.value() == 0.4321 && k == count() {
                k = i
            }
        }
    }
    k
}

pub fn loss_slope() -> f32 {
    slope(loss, radius())
}

pub fn frame(time: f32, width: u32, height: u32) {}
";
    let pkg = crate::scratch("measures-blueprint-src");
    crate::write_package(&pkg, "blueprinted", src);
    std::fs::write(pkg.join("blueprint.wrela"), blueprint).expect("write the blueprint");
    let dir =
        crate::lift::build_into("measures-blueprint", &pkg, &["blueprinted/main.wrela"], false);
    let mut host = wrela_host::CpuBuild::load(dir).expect("load").start().expect("start");
    let slope = wrela_tests::one_f32(&mut host, "loss_slope", &[]);
    println!("an outline from a blueprint's Vec: the loss's slope by the radius is {slope}");
    assert!(slope != 0.0 && slope.is_finite(), "{slope}");
}
