//! The engine's loft (engine::loft): a constant one is a capped cylinder near its side, its sign
//! is right everywhere, its distance has a gradient of 1 at the surface where its sections slope
//! (an ellipse's, turning and narrowing along the axis), its bound holds there, and its
//! interval (derived through its table, read at indices that depend on the point) encloses its
//! values.

#[test]
fn the_loft_is_a_distance_its_bound_and_interval_hold() {
    let dir = super::engine_package("loft", "lofts", PROGRAM);
    assert_eq!(super::tests_pass(&dir), 5);
}

const PROGRAM: &str = "use engine::loft::{Loft, loft}
use std::derive::{Box3, interval}
use std::field::Surface
use std::math::TAU

fn cylinder() -> Loft<96> {
    loft(vec3(0.0, 0.0, -0.2), vec3(0.0, 0.0, 0.3), vec3(y: 1.0), stations: 4, angles: 24, radii: [0.1; 96])
}

/// The exact distance to a capped cylinder of radius 0.1 along z from -0.2 to 0.3.
fn exact(p: vec3) -> f32 {
    let d = vec2(length(vec2(p.x, p.y)) - 0.1, abs(p.z - 0.05) - 0.25)
    min(max(d.x, d.y), 0.0) + length(max(d, vec2()))
}

/// Ellipses, 2:1, turning a quarter turn and narrowing from 0.12 to 0.06 along the axis: their
/// edges slope along the axis and around it.
fn ellipses() -> Loft<512> {
    var radii = [0.0; 512]
    for i in 0..16 {
        let t = f32(i) / 15.0
        let size = 0.12 - 0.06 * t
        for j in 0..32 {
            let a = TAU * f32(j) / 32.0 - 0.25 * TAU * t
            radii[i * 32 + j] = size / length(vec2(cos(a), 2.0 * sin(a)))
        }
    }
    loft(vec3(0.0, 0.0, -0.3), vec3(0.0, 0.0, 0.3), vec3(x: 1.0), stations: 16, angles: 32, radii: radii)
}

const AROUND: Box3 = Box3 { lo: vec3(-0.3, -0.3, -0.4), hi: vec3(0.3, 0.3, 0.5) }

@test
fn a_constant_loft_is_a_cylinder_near_its_side() {
    let f = cylinder()
    var worst = 0.0
    var n = 0
    for i in 0..4000 {
        let p = AROUND.sample(1, i)
        // Beside the side, 24 radii make a polygon within 0.1 (1 - cos 7.5°) = 0.86 mm.
        if p.z > -0.2 && p.z < 0.3 && abs(exact(p)) < 0.05 {
            worst = max(worst, abs(f.distance(p) - exact(p)))
            n += 1
        }
    }
    assert(n > 100 && worst < 0.0009)
}

@test
fn its_sign_is_right_everywhere() {
    let f = cylinder()
    for i in 0..4000 {
        let p = AROUND.sample(2, i)
        if abs(exact(p)) > 0.002 {
            assert((f.distance(p) < 0.0) == (exact(p) < 0.0))
        }
    }
}

@test
fn its_gradient_is_1_at_a_sloping_surface() {
    let f = ellipses()
    var n = 0
    for i in 0..20000 {
        let p = AROUND.sample(3, i)
        let (d, g) = f.sample(p)
        // On the side, away from the caps.
        if abs(d) < 0.001 && p.z > -0.29 && p.z < 0.29 {
            assert(abs(length(g) - 1.0) < 0.02)
            n += 1
        }
    }
    assert(n > 20)
}

@test
fn its_bound_holds_near_its_surface() {
    assert(cylinder().lipschitz(0.01) < 1.01)
    // 5 mm: the narrow end is 3 cm across, and at 10 mm the zone near its axis is in scope, so
    // the bound there is infinite (as it should be).
    let f = ellipses()
    assert(f.lipschitz(0.01) > 1.0e30)
    let l = f.lipschitz(0.005)
    assert(l < 2.0)
    for i in 0..20000 {
        let p = AROUND.sample(4, i)
        let (d, g) = f.sample(p)
        if abs(d) < 0.005 {
            assert(length(g) <= l)
        }
    }
}

@test
fn its_interval_holds_its_values() {
    let f = ellipses()
    for k in 0..200 {
        let c = AROUND.sample(5, k)
        let h = 0.002 + 0.05 * f32(k % 7) / 6.0
        let b = Box3 { lo: c - vec3(h), hi: c + vec3(h) }
        let r = interval(|q: vec3| f.distance(q), b)
        for i in 0..64 {
            let v = f.distance(b.sample(6, i))
            assert(v >= r.lo && v <= r.hi)
        }
    }
}

pub fn frame(time: f32, width: u32, height: u32) {}
";
