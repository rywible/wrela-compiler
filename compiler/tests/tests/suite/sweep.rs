//! The engine's sweep (engine::sweep): a straight one of round sections is a capped cylinder
//! near its side, and one along a quarter circle a quarter of a torus; its sign is right
//! everywhere; its frame is carried along its path; its sections reach as named, with their
//! bumps; its ends are flat or domed; its distance has a gradient near 1 at the surface of a
//! bent sweep whose sections change, its bound holds there, and its interval holds its values.
//! And the creature's crease join (engine::creature::crease) sinks a groove where two forms
//! meet, and nowhere else, within its stated steepness; and digits (engine::digits) are toes
//! apart from each other, their tips where they're placed, their gradient near 1.

#[test]
fn the_sweep_is_a_distance_its_bound_and_interval_hold() {
    let dir = super::engine_package("sweep", "sweeps", PROGRAM);
    assert_eq!(super::tests_pass(&dir), 11);
}

const PROGRAM: &str = "use engine::creature::crease
use engine::digits::{Digits, digits}
use engine::sweep::{Section, Sweep, round, section, sweep}
use std::derive::{Box3, gradient, interval}
use std::field::Surface
use std::math::{PI, TAU}

/// Round sections 0.1 from a straight path along z from -0.2 to 0.3: a capped cylinder.
fn cylinder() -> Sweep<2> {
    sweep(
        [vec3(0.0, 0.0, -0.2), vec3(0.0, 0.0, 0.3)],
        up: vec3(y: 1.0),
        sections: [round(0.1), round(0.1)],
        places: [0.0, 1.0],
    )
}

/// The exact distance to that cylinder.
fn exact_cylinder(p: vec3) -> f32 {
    let d = vec2(length(vec2(p.x, p.y)) - 0.1, abs(p.z - 0.05) - 0.25)
    min(max(d.x, d.y), 0.0) + length(max(d, vec2()))
}

/// Round sections 0.04 from a path through 9 points on a quarter of a circle of radius 0.2
/// about the y axis.
fn quarter() -> Sweep<1> {
    var points = [vec3(); 9]
    for i in 0..9 {
        let a = 0.25 * PI * f32(i) / 4.0
        points[i] = vec3(0.2 * cos(a), 0.0, 0.2 * sin(a))
    }
    sweep(points, up: vec3(y: 1.0), sections: [round(0.04)], places: [0.0])
}

fn exact_torus(p: vec3) -> f32 {
    length(vec2(length(vec2(p.x, p.z)) - 0.2, p.y)) - 0.04
}

/// A leg: down from a hip, bending back at a knee and forward at an ankle to a paw, its
/// sections changing (an oval thigh with a muscle's bump, a narrower shin, a flat paw), its
/// paw's end domed.
fn leg() -> Sweep<4> {
    sweep(
        [vec3(0.0, 0.6, 0.0), vec3(0.0, 0.35, 0.05), vec3(0.0, 0.15, -0.04), vec3(0.0, 0.045, 0.0), vec3(0.0, 0.03, 0.1)],
        up: vec3(z: 1.0),
        sections: [
            section(up: 0.07, down: 0.06, left: 0.045, right: 0.04, square: 2.4)
                .bump(at: 0.6, height: 0.008, width: 0.5),
            section(up: 0.035, down: 0.03, left: 0.025, right: 0.025, turn: 0.2)
                .bump(at: 2.5, height: -0.004, width: 0.4),
            section(up: 0.02, down: 0.02, left: 0.018, right: 0.018),
            section(up: 0.018, down: 0.012, left: 0.025, right: 0.025, square: 3.0),
        ],
        places: [0.0, 0.4, 0.75, 1.0],
        ends: vec2(0.0, 0.02),
    )
}

const AROUND: Box3 = Box3 { lo: vec3(-0.3, -0.3, -0.4), hi: vec3(0.3, 0.3, 0.5) }
const LEG: Box3 = Box3 { lo: vec3(-0.15, -0.05, -0.15), hi: vec3(0.15, 0.7, 0.2) }

@test
fn a_straight_sweep_is_a_cylinder_and_a_bent_one_a_torus() {
    let c = cylinder()
    var n = 0
    for i in 0..4000 {
        let p = AROUND.sample(1, i)
        if p.z > -0.19 && p.z < 0.29 && abs(exact_cylinder(p)) < 0.05 {
            assert(abs(c.distance(p) - exact_cylinder(p)) < 0.0005)
            n += 1
        }
    }
    assert(n > 100)
    let t = quarter()
    var m = 0
    let near = Box3 { lo: vec3(-0.05, -0.1, -0.05), hi: vec3(0.3, 0.1, 0.3) }
    for i in 0..8000 {
        let p = near.sample(2, i)
        let a = atan2(p.z, p.x)
        if a > 0.1 && a < 0.25 * PI - 0.1 && abs(exact_torus(p)) < 0.02 {
            assert(abs(t.distance(p) - exact_torus(p)) < 0.0005)
            m += 1
        }
    }
    assert(m > 100)
}

@test
fn its_sign_is_right_everywhere() {
    let c = cylinder()
    for i in 0..4000 {
        let p = AROUND.sample(3, i)
        if abs(exact_cylinder(p)) > 0.002 {
            assert((c.distance(p) < 0.0) == (exact_cylinder(p) < 0.0))
        }
    }
    let t = quarter()
    for i in 0..4000 {
        let p = AROUND.sample(4, i)
        let a = atan2(p.z, p.x)
        if a > 0.05 && a < 0.25 * PI - 0.05 && abs(exact_torus(p)) > 0.002 {
            assert((t.distance(p) < 0.0) == (exact_torus(p) < 0.0))
        }
    }
}

@test
fn its_frame_is_carried_along_its_path() {
    // At the hip, angle 0 is the leg's front (+z); at the paw, which runs forward, its top.
    let l = leg()
    assert(abs(l.place(vec3(0.0, 0.58, 0.05)).theta) < 0.05)
    assert(abs(l.place(vec3(0.0, 0.07, 0.05)).theta) < 0.1)
    // The left is +x: angle π/2 (in the hip's plane, square to the path there).
    assert(abs(l.place(vec3(0.05, 0.6, 0.0)).theta - 0.5 * PI) < 0.001)
}

@test
fn its_sections_reach_as_named() {
    let s = section(up: 0.05, down: 0.03, left: 0.04, right: 0.02)
    assert(abs(s.radius(0.0) - 0.05) < 0.000001)
    assert(abs(s.radius(0.5 * PI) - 0.04) < 0.000001)
    assert(abs(s.radius(PI) - 0.03) < 0.000001)
    assert(abs(s.radius(1.5 * PI) - 0.02) < 0.000001)
    // Squarer quarters reach further between the extents.
    let r = 0.25 * PI
    let squarer = section(up: 0.04, down: 0.04, left: 0.04, right: 0.04, square: 4.0)
    assert(squarer.radius(r) > round(0.04).radius(r) + 0.004)
    // A bump adds its height at its angle, and nothing past its width.
    let b = round(0.04).bump(at: 1.0, height: 0.006, width: 0.3)
    assert(abs(b.radius(1.0) - 0.046) < 0.000001)
    assert(abs(b.radius(1.31) - 0.04) < 0.000001)
    assert(b.radius(1.15) > 0.04 && b.radius(1.15) < 0.046)
    // Turned: its up extent turns toward the left.
    let t = section(up: 0.05, down: 0.03, left: 0.04, right: 0.02, turn: 0.3)
    assert(abs(t.radius(0.3) - 0.05) < 0.000001)
}

@test
fn its_ends_are_flat_or_domed() {
    let l = leg()
    // The hip's end is flat: the surface just past it is its plane.
    let hip = l.place(vec3(0.0, 0.6, 0.0))
    assert(abs(hip.along) < 0.000001)
    assert(abs(l.distance(vec3(0.0, 0.605, 0.0)) - 0.005) < 0.0005)
    // The paw's dome reaches 2 cm past its end.
    let tip = vec3(0.0, 0.03, 0.1)
    let d = l.distance(tip + vec3(0.0, 0.0, 0.02))
    assert(abs(d) < 0.001)
    assert(l.distance(tip + vec3(0.0, 0.0, 0.03)) > 0.005)
}

@test
fn its_gradient_is_near_1_at_its_surface() {
    let l = leg()
    var n = 0
    var worst = 0.0
    for i in 0..60000 {
        let p = LEG.sample(5, i)
        // On the side, away from the ends.
        if abs(l.distance(p)) < 0.001 {
            let (d, g) = l.sample(p)
            let pl = l.place(p)
            if pl.along > 0.02 && pl.along < l.length() - 0.02 {
                worst = max(worst, abs(length(g) - 1.0))
                n += 1
            }
        }
    }
    assert(n > 100)
    assert(worst < 0.15, f\"the gradient's length is {worst} from 1\")
}

@test
fn its_bound_holds_near_its_surface() {
    let l = leg()
    let b = l.lipschitz(0.004)
    assert(b < 1.0e30, f\"{b}\")
    for i in 0..40000 {
        let p = LEG.sample(6, i)
        let (d, g) = l.sample(p)
        if abs(d) < 0.004 {
            assert(length(g) <= b)
        }
    }
    // Wide enough to reach the path, it's infinite; and where a turn is sharper than the
    // sweep is thick, so its inside folds over itself.
    assert(l.lipschitz(0.05) > 1.0e30)
    let folded = sweep(
        [vec3(0.0, 0.3, 0.0), vec3(0.0, 0.03, 0.0), vec3(0.0, 0.025, 0.06)],
        up: vec3(z: 1.0),
        sections: [round(0.03)],
        places: [0.0],
    )
    assert(folded.lipschitz(0.001) > 1.0e30)
}

@test
fn its_interval_holds_its_values() {
    let l = leg()
    for k in 0..100 {
        let c = LEG.sample(7, k)
        let h = 0.002 + 0.05 * f32(k % 7) / 6.0
        let b = Box3 { lo: c - vec3(h), hi: c + vec3(h) }
        let r = interval(|q: vec3| l.distance(q), b)
        for i in 0..32 {
            let v = l.distance(b.sample(8, i))
            assert(v >= r.lo && v <= r.hi)
        }
    }
}

@test
fn a_section_between_two_is_between_them() {
    let s = sweep(
        [vec3(), vec3(0.0, 0.0, 1.0)],
        up: vec3(y: 1.0),
        sections: [round(0.02), section(up: 0.06, down: 0.06, left: 0.04, right: 0.04)],
        places: [0.0, 1.0],
    )
    let m = s.section_at(0.5)
    assert(abs(m.up - 0.04) < 0.000001 && abs(m.left - 0.03) < 0.000001)
}

fn left_ball(p: vec3) -> f32 {
    length(p - vec3(-0.06, 0.0, 0.0)) - 0.1
}

fn right_ball(p: vec3) -> f32 {
    length(p - vec3(0.06, 0.0, 0.0)) - 0.1
}

/// Two balls 0.1 across, 0.12 apart, joined by a crease 2 cm wide and 5 mm deep.
fn creased(p: vec3) -> f32 {
    crease(0.02, 0.005).apply(left_ball(p), right_ball(p), 0.0)
}

@test
fn a_crease_sinks_where_two_forms_meet() {
    // On the circle where the balls meet (x = 0, 8 cm from the axis), the union's surface:
    // creased, it's 5 mm inside the surface there.
    assert(abs(creased(vec3(0.0, 0.08, 0.0)) - 0.005) < 0.0001)
    // On the far side of a ball, it's the union; deep inside where they overlap (4 cm), it's
    // raised by the depth at most, and still inside.
    let far = vec3(-0.16, 0.0, 0.0)
    assert(abs(creased(far) - min(left_ball(far), right_ball(far))) < 0.000001)
    let inside = creased(vec3())
    assert(inside < -0.03 && inside <= min(left_ball(vec3()), right_ball(vec3())) + 0.0050001)
    // Its gradient stays within its steepness near the surface.
    let most = crease(0.02, 0.005).steepness()
    let around = Box3 { lo: vec3(-0.2), hi: vec3(0.2) }
    var n = 0
    for i in 0..20000 {
        let p = around.sample(9, i)
        if abs(creased(p)) < 0.01 {
            assert(length(gradient(|q: vec3| creased(q), p)) <= most * 1.0001)
            n += 1
        }
    }
    assert(n > 500)
}

/// A wolf's fore paw's four toes: 4 cm across, 3.5 cm long, 9 mm thick, pointing along +z.
fn paw() -> Digits<4> {
    digits(vec3(0.0, 0.02, 0.0), forward: vec3(z: 1.0), up: vec3(y: 1.0), spread: 0.04, length: 0.035, thick: 0.009)
}

@test
fn digits_are_toes_apart() {
    let p = paw()
    // Each toe's path, halfway, is inside it, and the gap between two toes' tips, at their
    // height, is outside every toe.
    for i in 0..4 {
        assert(p.distance(p.toes[i].at[p.toes[i].vertices / 2]) < -0.005)
    }
    let a = p.toes[1].at[p.toes[1].vertices - 1]
    let b = p.toes[2].at[p.toes[2].vertices - 1]
    assert(p.distance(0.5 * (a + b)) > 0.0, f\"between the tips: {p.distance(0.5 * (a + b))}\")
    // The outer toes are shorter: their tips are behind the middle ones'.
    let outer = p.toes[0].at[p.toes[0].vertices - 1]
    assert(outer.z < a.z - 0.002, f\"tips {outer.z} {a.z}\")
    // Near its surface, its gradient is near 1: down to 0.6 at a tip's dome, whose distance
    // (an ellipse's, scaled by its shorter half-axis) never overstates.
    let around = Box3 { lo: vec3(-0.04, 0.0, -0.01), hi: vec3(0.04, 0.04, 0.05) }
    var n = 0
    for i in 0..40000 {
        let q = around.sample(10, i)
        if abs(p.distance(q)) < 0.0005 {
            let (d, g) = p.sample(q)
            assert(length(g) > 0.6 && length(g) < 1.2, f\"gradient {length(g)}\")
            n += 1
        }
    }
    assert(n > 100, f\"{n} near\")
    assert(p.lipschitz(0.001) < 1.0e30, f\"bound {p.lipschitz(0.001)}\")
}

pub fn frame(time: f32, width: u32, height: u32) {}
";
