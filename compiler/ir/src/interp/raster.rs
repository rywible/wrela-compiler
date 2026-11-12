//! Where a triangle covers a pixel, and what its fragment there is given: coverage by
//! homogeneous edge functions (Olano and Greer, "Triangle scan conversion using 2D homogeneous
//! coordinates", 1997), so a triangle that crosses the near plane needs no clipping; the
//! perspective-correct weights of its corners there; its depth; and its varyings, interpolated
//! (a `Flat` one and an integer one taken from the first corner, WebGPU's provoking vertex).

use super::V;
use crate::*;

/// Where a triangle meets a pixel's centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// Each corner's weight at the point, perspective-correct (they sum to 1).
    pub weights: [f64; 3],
    /// The depth (`z / w`, through the viewport's depth range).
    pub depth: f64,
    /// `1 / w` at the point.
    pub inv_w: f64,
    /// Whether the point is inside the triangle (else it's on the triangle's plane outside it:
    /// a helper invocation of a block of four).
    pub inside: bool,
    /// Front-facing (counter-clockwise on the screen, WebGPU's default).
    pub front: bool,
}

/// The viewport: its corner and size in pixels, and its depth range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

impl Viewport {
    /// The whole of a target `w` × `h`.
    pub fn whole(w: u32, h: u32) -> Viewport {
        Viewport {
            x: 0.0,
            y: 0.0,
            width: w as f32,
            height: h as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }
}

/// Where the triangle with clip positions `clip` meets the point `(px, py)` in pixels (a
/// pixel's centre is at `x + 0.5`), on the triangle's plane: `None` if the plane is seen edge
/// on there.
pub fn hit(clip: [[f32; 4]; 3], px: f64, py: f64, vp: &Viewport) -> Option<Hit> {
    // The point in normalized device coordinates (y up).
    let sx = (px - f64::from(vp.x)) / f64::from(vp.width) * 2.0 - 1.0;
    let sy = 1.0 - (py - f64::from(vp.y)) / f64::from(vp.height) * 2.0;
    let c = clip.map(|p| p.map(f64::from));
    // M's columns are the corners' (x, y, w): solve M k = (sx, sy, 1). k is each corner's
    // weight over the interpolated w.
    let m = [[c[0][0], c[1][0], c[2][0]], [c[0][1], c[1][1], c[2][1]], [c[0][3], c[1][3], c[2][3]]];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let p = [sx, sy, 1.0];
    // Cramer's rule.
    let col = |i: usize| -> f64 {
        let mut n = m;
        for r in 0..3 {
            n[r][i] = p[r];
        }
        n[0][0] * (n[1][1] * n[2][2] - n[1][2] * n[2][1])
            - n[0][1] * (n[1][0] * n[2][2] - n[1][2] * n[2][0])
            + n[0][2] * (n[1][0] * n[2][1] - n[1][1] * n[2][0])
    };
    let k = [col(0) / det, col(1) / det, col(2) / det];
    let sum = k[0] + k[1] + k[2];
    if sum == 0.0 {
        return None;
    }
    let weights = k.map(|x| x / sum);
    // In front of the eye (w > 0 at the point) and within all three edges.
    let w = weights[0] * c[0][3] + weights[1] * c[1][3] + weights[2] * c[2][3];
    let inside = w > 0.0 && weights.iter().all(|&x| x >= 0.0);
    let z = (weights[0] * c[0][2] + weights[1] * c[1][2] + weights[2] * c[2][2]) / w;
    let depth = f64::from(vp.min_depth) + z * f64::from(vp.max_depth - vp.min_depth);
    // The screen's winding: the sign of the determinant, with each corner's w's sign.
    let front = det * c[0][3].signum() * c[1][3].signum() * c[2][3].signum() > 0.0;
    Some(Hit { weights, depth, inv_w: 1.0 / w, inside, front })
}

/// A vertex shader's clip position, from its result: the result itself (a `ClipPosition`), or
/// its member at `varyings`' position path.
pub fn clip_of(result: &V, varyings: &[Varying]) -> Option<[f32; 4]> {
    let at = varyings.iter().find(|v| v.position).map_or(&[][..], |v| v.path.as_slice());
    let mut v = result;
    for &k in at {
        match v {
            V::P(ps) => v = ps.get(k as usize)?,
            V::S(_) => return None,
        }
    }
    // A `ClipPosition` is a struct of its `position`.
    if let V::P(ps) = v
        && ps.len() == 1
    {
        v = &ps[0];
    }
    let xs = super::concrete::floats_of(v);
    (xs.len() == 4).then(|| [xs[0], xs[1], xs[2], xs[3]])
}

/// The fragment's varyings: the vertices' results, each float interpolated with `hit`'s
/// weights, except a flat member's and an integer (the first corner's), and the clip position's
/// member, which is the fragment's position `(x, y, depth, 1 / w)`.
pub fn interpolate(results: [&V; 3], varyings: &[Varying], hit: &Hit, at: [f32; 2]) -> V {
    fn walk(path: &mut Vec<u32>, vs: [&V; 3], hit: &Hit, varyings: &[Varying], pos: &V) -> V {
        if let Some(v) = varyings.iter().find(|v| v.path == *path) {
            if v.position {
                // As the member is held: a `ClipPosition` (a struct of one `vec4`), or the
                // `vec4` itself.
                return match vs[0] {
                    V::P(p) if p.len() == 1 && matches!(p[0], V::P(_)) => V::P(vec![pos.clone()]),
                    _ => pos.clone(),
                };
            }
            if v.flat {
                return vs[0].clone();
            }
        }
        match vs {
            [V::P(a), V::P(b), V::P(c)] => V::P(
                (0..a.len())
                    .map(|i| {
                        path.push(i as u32);
                        let r = walk(path, [&a[i], &b[i], &c[i]], hit, varyings, pos);
                        path.pop();
                        r
                    })
                    .collect(),
            ),
            [V::S(Const::F32(a)), V::S(Const::F32(b)), V::S(Const::F32(c))] => {
                let w = hit.weights;
                V::S(Const::F32(
                    (w[0] * f64::from(*a) + w[1] * f64::from(*b) + w[2] * f64::from(*c)) as f32,
                ))
            }
            [first, ..] => first.clone(),
        }
    }
    let pos = super::concrete::fvec(&[at[0], at[1], hit.depth as f32, hit.inv_w as f32]);
    walk(&mut Vec::new(), results, hit, varyings, &pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_triangles_weights_are_perspective_correct() {
        let vp = Viewport::whole(100, 100);
        // The third corner has w = 2, at the same place on the screen as (-0.9, 0.9) with w 1.
        let clip = [[-0.9, -0.9, 0.5, 1.0], [0.9, -0.9, 0.5, 1.0], [-1.8, 1.8, 1.0, 2.0]];
        // At the screen-space midpoint of the left edge, the perspective-correct weight of the
        // far corner is less than a half (it's nearer the w = 1 corner in clip space).
        let mid_y = 50.0;
        let h = hit(clip, 5.0 + 0.5, mid_y, &vp).expect("a hit");
        assert!(h.inside);
        assert!(h.weights[2] < 0.5, "{:?}", h.weights);
        let s: f64 = h.weights.iter().sum();
        assert!((s - 1.0).abs() < 1e-12);
        // Outside: the top right of the screen.
        assert!(!hit(clip, 95.0, 5.0, &vp).expect("a hit").inside);
    }
}
