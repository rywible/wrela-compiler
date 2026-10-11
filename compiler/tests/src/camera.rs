//! A frame's camera as a program reports it (the examples' `test_camera`), and the vector
//! arithmetic the image tests do with it: a pixel's world point from its depth, and a world
//! point's pixel.

/// The camera a frame was drawn by (an example's `CameraReport`): its reversed-depth
/// perspective, its jitter in clip units, and the scene's size.
#[derive(Clone, Copy, Debug)]
pub struct Cam {
    pub eye: [f64; 3],
    pub forward: [f64; 3],
    pub right: [f64; 3],
    pub up: [f64; 3],
    pub tan_half: f64,
    pub aspect: f64,
    pub near: f64,
    pub jitter: [f64; 2],
    pub screen: [f64; 2],
}

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add_scaled(a: [f64; 3], b: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}

pub fn len(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

impl Cam {
    /// The camera from a `test_camera` export's floats: eye, forward, right, up, the tangent of
    /// half the vertical field of view, the aspect, the near plane, the jitter and the scene's
    /// size.
    pub fn of(v: &[f64]) -> Cam {
        let v3 = |i: usize| [v[i], v[i + 1], v[i + 2]];
        Cam {
            eye: v3(0),
            forward: v3(3),
            right: v3(6),
            up: v3(9),
            tan_half: v[12],
            aspect: v[13],
            near: v[14],
            jitter: [v[15], v[16]],
            screen: [v[17], v[18]],
        }
    }

    /// The ray through pixel `px` (scene pixels, its centre at +0.5), unit length.
    pub fn ray(&self, px: [f64; 2]) -> [f64; 3] {
        let ndc = [
            px[0] / self.screen[0] * 2.0 - 1.0 - self.jitter[0],
            1.0 - px[1] / self.screen[1] * 2.0 - self.jitter[1],
        ];
        let mut ray = add_scaled(self.forward, self.right, ndc[0] * self.tan_half * self.aspect);
        ray = add_scaled(ray, self.up, ndc[1] * self.tan_half);
        let l = len(ray);
        ray.map(|x| x / l)
    }

    /// How far along pixel `px`'s ray its depth `z` (reversed) is.
    pub fn distance_at(&self, px: [f64; 2], z: f64) -> f64 {
        let ray = self.ray(px);
        (self.near / z.max(1e-12)) / dot(ray, self.forward)
    }

    /// The world point at pixel `px` (scene pixels, its centre at +0.5) of depth `z` (reversed).
    pub fn world_at(&self, px: [f64; 2], z: f64) -> [f64; 3] {
        let ray = self.ray(px);
        let dist = self.near / z.max(1e-12);
        add_scaled(self.eye, ray, dist / dot(ray, self.forward))
    }

    /// Where `p` falls on a target `size` wide and high (pixels, jittered as this frame drew),
    /// and its depth there (reversed); none behind the eye.
    pub fn project(&self, p: [f64; 3], size: [f64; 2]) -> Option<([f64; 2], f64)> {
        let v = sub(p, self.eye);
        let z = dot(v, self.forward);
        if z <= self.near {
            return None;
        }
        let x = dot(v, self.right) / (z * self.tan_half * self.aspect) + self.jitter[0];
        let y = dot(v, self.up) / (z * self.tan_half) + self.jitter[1];
        Some(([(x + 1.0) * 0.5 * size[0], (1.0 - y) * 0.5 * size[1]], self.near / z))
    }
}

/// A frame as the flicker measure reads it: the output's pixels (RGBA8), the scene's depth
/// (reversed, at the scene's size), its camera, and the output's tags (engine::view's: a class
/// times 64, plus which one).
pub struct Seen<'a> {
    pub screen: &'a [u8],
    pub depth: &'a [f32],
    pub cam: &'a Cam,
    pub tags: &'a [f32],
}

/// A frame grabbed whole: its output's pixels, its depth, its camera and its tags.
pub type Frame = (Vec<u8>, Vec<f32>, Cam, Vec<f32>);

impl<'a> Seen<'a> {
    pub fn of(f: &'a Frame) -> Seen<'a> {
        Seen { screen: &f.0, depth: &f.1, cam: &f.2, tags: &f.3 }
    }
}

/// A tag's class (engine::view's).
pub fn class(tag: f32) -> u32 {
    (tag.max(0.0) / 64.0 + 0.001) as u32
}

/// What `warp_error` found over a pair of frames: the mean warp error (/255), the share of
/// pixels over 8/255, the pixels compared and their mean motion (output pixels).
pub struct Warp {
    pub mean: f64,
    pub over: f64,
    pub n: usize,
    pub motion: f64,
}

/// Frame `b` warped into frame `a` (spike 12's flicker measure), on outputs `size` wide and
/// high, every other pixel each way: each output pixel of `b`, at its depth (the scene's sample
/// nearest it), seen by `a`'s camera, against `a` there, sampled bilinearly. Pixels of a class in
/// `left_out` (in either frame), or disoccluded (a's depth not within 2%), are left out. Each
/// class's sum and count are added to `by`.
pub fn warp_error(
    a: &Seen,
    b: &Seen,
    size: (usize, usize),
    left_out: &[u32],
    by: &mut [(f64, usize)],
) -> Warp {
    let (w, h) = size;
    let (sw, sh) = (b.cam.screen[0] as usize, b.cam.screen[1] as usize);
    let scale = w as f64 / b.cam.screen[0];
    let unjittered = |c: &Cam| Cam { jitter: [0.0, 0.0], ..*c };
    let (ua, ub) = (unjittered(a.cam), unjittered(b.cam));
    let out = |tag: f32| left_out.contains(&class(tag));
    let (mut sum, mut over, mut n, mut motion) = (0.0, 0usize, 0usize, 0.0);
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let o = y * w + x;
            if out(b.tags[o]) {
                continue;
            }
            // The output pixel's point in b: its unjittered ray at the nearest sample's depth.
            let (qx, qy) =
                (((x as f64 + 0.5) / scale) as usize, ((y as f64 + 0.5) / scale) as usize);
            let z = f64::from(b.depth[qy.min(sh - 1) * sw + qx.min(sw - 1)]);
            if z <= 0.0 {
                continue;
            }
            let p = ub.world_at([(x as f64 + 0.5) / scale, (y as f64 + 0.5) / scale], z);
            let Some((pa, za)) = ua.project(p, [w as f64, h as f64]) else { continue };
            let (ax, ay) = (pa[0].floor(), pa[1].floor());
            if ax < 0.0 || ay < 0.0 || ax >= w as f64 || ay >= h as f64 {
                continue;
            }
            if out(a.tags[ay as usize * w + ax as usize]) {
                continue;
            }
            // A sees the same surface there (its depth within 2%), as spike 12 asks.
            let (sx, sy) = ((pa[0] / scale) as usize, (pa[1] / scale) as usize);
            let zs = f64::from(a.depth[sy.min(sh - 1) * sw + sx.min(sw - 1)]);
            if (zs - za).abs() > za * 0.02 {
                continue;
            }
            // A sampled bilinearly, as spike 12 samples it: the measure includes resampling's
            // blur, the same floor for every look.
            let (fx, fy) = ((pa[0] - 0.5).max(0.0), (pa[1] - 0.5).max(0.0));
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
            let at = |x: usize, y: usize, ch: usize| f64::from(a.screen[4 * (y * w + x) + ch]);
            let mut most = 0.0f64;
            let mut d3 = 0.0;
            for ch in 0..3 {
                let top = at(x0, y0, ch) * (1.0 - tx) + at(x1, y0, ch) * tx;
                let bottom = at(x0, y1, ch) * (1.0 - tx) + at(x1, y1, ch) * tx;
                let v = top * (1.0 - ty) + bottom * ty;
                let d = (v - f64::from(b.screen[4 * o + ch])).abs();
                d3 += d;
                most = most.max(d);
            }
            sum += d3 / 3.0;
            if most > 8.0 {
                over += 1;
            }
            let k = class(b.tags[o]) as usize;
            if k < by.len() {
                by[k].0 += d3 / 3.0;
                by[k].1 += 1;
            }
            n += 1;
            motion += ((pa[0] - x as f64 - 0.5).powi(2) + (pa[1] - y as f64 - 0.5).powi(2)).sqrt();
        }
    }
    let nf = n.max(1) as f64;
    Warp { mean: sum / nf, over: over as f64 / nf, n, motion: motion / nf }
}
