//! Measures and specs (language.md §22): std::field's measures find a surface where it is, to
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
