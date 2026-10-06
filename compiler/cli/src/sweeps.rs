//! Sweeps for `wrela reference replica`: a part with `sections` is a sweep (engine/sweep.wrela)
//! along a smooth path from its `from` through its `through` to its `to`. Its radii are measured
//! in the sweep's own section planes (where a ray from the path leaves the mesh, as a loft's
//! are), and its control sections, named (extents, squareness, turn) with up to four bumps,
//! fitted to them by least squares. The path, its planes and its sections' blend are the
//! engine's, written again here, so the planes measured are the planes the engine draws.

use crate::mesh::{self, Solid, V3};
use crate::reference::Banded;

/// The most vertices a sweep's path has (the engine's `VERTICES`).
pub const VERTICES: usize = 33;

/// A section across a sweep, as the engine's: extents up (angle 0), left, down and right;
/// squareness; turn; and four bumps, each (angle, height, half-width), height 0 for none.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Section {
    pub up: f64,
    pub left: f64,
    pub down: f64,
    pub right: f64,
    pub square: f64,
    pub turn: f64,
    pub bumps: [[f64; 3]; 4],
}

/// `a` as an angle in [-π, π).
fn wrap(a: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    a - tau * ((a + std::f64::consts::PI) / tau).floor()
}

impl Section {
    pub fn round(r: f64) -> Section {
        Section { up: r, left: r, down: r, right: r, square: 2.0, ..Section::default() }
    }

    /// Its radius at angle `theta` (radians from up, toward the left): the engine's.
    pub fn radius(&self, theta: f64) -> f64 {
        let n = self.square.max(1.0);
        self.radius_with(theta, n, |_, a, b| least(a, b, n))
    }

    /// Ready to give its radius at many angles: `Outline::radius` is `radius`, with what its
    /// numbers alone decide worked out once.
    pub fn outline(&self) -> Outline<'_> {
        let n = self.square.max(1.0);
        let (up, down) = (self.up.max(0.0001), self.down.max(0.0001));
        let (left, right) = (self.left.max(0.0001), self.right.max(0.0001));
        let least =
            [least(up, left, n), least(up, right, n), least(down, left, n), least(down, right, n)];
        Outline { section: self, n, least }
    }

    /// `radius`, with its squareness `n` and its quadrant's least sum (`least`, given the
    /// quadrant, 0 to 3 from up-left to down-right, and its extents).
    fn radius_with(&self, theta: f64, n: f64, least: impl Fn(usize, f64, f64) -> f64) -> f64 {
        let phi = theta - self.turn;
        let (c, s) = (phi.cos(), phi.sin());
        let a = if c >= 0.0 { self.up } else { self.down }.max(0.0001);
        let b = if s >= 0.0 { self.left } else { self.right }.max(0.0001);
        let quadrant = 2 * usize::from(c < 0.0) + usize::from(s < 0.0);
        let x = (c.abs() / a).max(0.000001);
        let y = (s.abs() / b).max(0.000001);
        let mut r = 1.0 / (x.powf(n) + y.powf(n)).max(least(quadrant, a, b)).powf(1.0 / n);
        for k in &self.bumps {
            let d = wrap(theta - k[0]).abs();
            if d < k[2] {
                let q = (0.5 * std::f64::consts::PI * d / k[2].max(0.0001)).cos();
                r += k[1] * q * q;
            }
        }
        r.max(0.0)
    }

    /// Its numbers, the named six and then `bumps` bumps' three.
    fn params(&self, bumps: usize) -> Vec<f64> {
        let mut p = vec![self.up, self.left, self.down, self.right, self.square, self.turn];
        for b in &self.bumps[..bumps] {
            p.extend(b);
        }
        p
    }

    fn from_params(p: &[f64]) -> Section {
        let mut s = Section {
            up: p[0],
            left: p[1],
            down: p[2],
            right: p[3],
            square: p[4],
            turn: p[5],
            ..Section::default()
        };
        for (k, b) in p[6..].chunks(3).enumerate() {
            s.bumps[k] = [b[0], b[1], b[2]];
        }
        s
    }
}

/// The least xⁿ + yⁿ over a quadrant whose extents are `a` and `b`, as the engine's `radius`
/// holds it (there, so a derived interval stays finite).
fn least(a: f64, b: f64, n: f64) -> f64 {
    a.max(b).powf(-n) * 1f64.min(2f64.powf(1.0 - 0.5 * n))
}

/// A section's radius at many angles (`Section::outline`).
pub struct Outline<'a> {
    section: &'a Section,
    n: f64,
    /// Each quadrant's `least`, up-left, up-right, down-left, down-right.
    least: [f64; 4],
}

impl Outline<'_> {
    /// `Section::radius`, the same number.
    pub fn radius(&self, theta: f64) -> f64 {
        self.section.radius_with(theta, self.n, |q, _, _| self.least[q])
    }
}

/// The engine's blend of four sections by `w`, bumps matched by place, their angles blended
/// around `b`'s.
fn blend(a: &Section, b: &Section, c: &Section, d: &Section, w: [f64; 4]) -> Section {
    let mix = |f: fn(&Section) -> f64| w[0] * f(a) + w[1] * f(b) + w[2] * f(c) + w[3] * f(d);
    let mut out = Section {
        up: mix(|s| s.up),
        left: mix(|s| s.left),
        down: mix(|s| s.down),
        right: mix(|s| s.right),
        square: mix(|s| s.square),
        turn: mix(|s| s.turn),
        bumps: [[0.0; 3]; 4],
    };
    for i in 0..4 {
        // An empty slot takes the angle and width of a section that fills it, as the engine's.
        let like = slot(b, i, slot(c, i, slot(a, i, d.bumps[i])));
        let (ka, kb, kc, kd) =
            (slot(a, i, like), slot(b, i, like), slot(c, i, like), slot(d, i, like));
        let at = kb[0];
        let angle =
            at + w[0] * wrap(ka[0] - at) + w[2] * wrap(kc[0] - at) + w[3] * wrap(kd[0] - at);
        let h = w[0] * ka[1] + w[1] * kb[1] + w[2] * kc[1] + w[3] * kd[1];
        let wd = w[0] * ka[2] + w[1] * kb[2] + w[2] * kc[2] + w[3] * kd[2];
        out.bumps[i] = [angle, h, wd.max(0.0001)];
    }
    out
}

/// `s`'s bump in slot `i`, or, if it's empty, `like`'s angle and width at no height.
fn slot(s: &Section, i: usize, like: [f64; 3]) -> [f64; 3] {
    let k = s.bumps[i];
    if k[1] == 0.0 && k[2] == 0.0 { [like[0], 0.0, like[2]] } else { k }
}

fn catmull_rom(f: f64) -> [f64; 4] {
    let (f2, f3) = (f * f, f * f * f);
    [
        -0.5 * f3 + f2 - 0.5 * f,
        1.5 * f3 - 2.5 * f2 + 1.0,
        -1.5 * f3 + 2.0 * f2 + 0.5 * f,
        0.5 * f3 - 0.5 * f2,
    ]
}

/// The control section a station `f` of the way along the path takes most from, and the four
/// it takes from (`m - 1` to `m + 2`, held to the sections there are).
fn span(places: &[f64], f: f64) -> usize {
    let mut m = 0;
    for (i, &p) in places.iter().enumerate().take(places.len().saturating_sub(1)) {
        if f >= p {
            m = i;
        }
    }
    m
}

/// The section `f` of the way along the path: the engine's `section_at`.
pub fn section_at(sections: &[Section], places: &[f64], f: f64) -> Section {
    let n = sections.len();
    if n == 1 || f <= places[0] {
        return sections[0];
    }
    if f >= places[n - 1] {
        return sections[n - 1];
    }
    let m = span(places, f);
    let x = ((f - places[m]) / (places[m + 1] - places[m])).clamp(0.0, 1.0);
    let (b, c) = (&sections[m], &sections[m + 1]);
    let a = if m == 0 { blend(c, b, c, c, [-1.0, 2.0, 0.0, 0.0]) } else { sections[m - 1] };
    let d = if m + 2 == n { blend(b, c, b, b, [-1.0, 2.0, 0.0, 0.0]) } else { sections[m + 2] };
    blend(&a, b, c, &d, catmull_rom(x))
}

// ---- the path ---------------------------------------------------------------------------------

/// A sweep's path, cut into straight pieces, as the engine cuts it.
pub struct Path {
    pub at: Vec<V3>,
    pub normal: Vec<V3>,
    pub up: Vec<V3>,
    pub along: Vec<f64>,
}

fn lerp(a: V3, b: V3, t: f64) -> V3 {
    mesh::add(a, mesh::scale(mesh::sub(b, a), t))
}

fn unit(v: V3) -> V3 {
    mesh::scale(v, 1.0 / mesh::length(v).max(1e-9))
}

/// `u` turned as the turn from unit `from` to unit `to` turns it.
fn carry(u: V3, from: V3, to: V3) -> V3 {
    let axis = mesh::cross(from, to);
    let s = mesh::length(axis);
    let c = mesh::dot(from, to);
    if s < 0.000001 {
        return u;
    }
    let k = mesh::scale(axis, 1.0 / s);
    mesh::add(
        mesh::add(mesh::scale(u, c), mesh::scale(mesh::cross(k, u), s)),
        mesh::scale(k, mesh::dot(k, u) * (1.0 - c)),
    )
}

impl Path {
    /// The engine's path through `points`, angle 0 toward `up` at its start.
    pub fn new(points: &[V3], up: V3) -> Path {
        let p = points.len();
        assert!((2..=VERTICES).contains(&p));
        let per = if p == 2 { 1 } else { 8.min((VERTICES - 1) / (p - 1)) };
        let mut at = Vec::new();
        for i in 0..p - 1 {
            let (p1, p2) = (points[i], points[i + 1]);
            let p0 = if i == 0 { mesh::sub(mesh::scale(p1, 2.0), p2) } else { points[i - 1] };
            let p3 = if i + 2 == p { mesh::sub(mesh::scale(p2, 2.0), p1) } else { points[i + 2] };
            let k0 = 0.0;
            let k1 = k0 + mesh::length(mesh::sub(p1, p0)).sqrt().max(0.0001);
            let k2 = k1 + mesh::length(mesh::sub(p2, p1)).sqrt().max(0.0001);
            let k3 = k2 + mesh::length(mesh::sub(p3, p2)).sqrt().max(0.0001);
            for j in 0..per {
                let t = k1 + (k2 - k1) * j as f64 / per as f64;
                let a1 = lerp(p0, p1, (t - k0) / (k1 - k0));
                let a2 = lerp(p1, p2, (t - k1) / (k2 - k1));
                let a3 = lerp(p2, p3, (t - k2) / (k3 - k2));
                let b1 = lerp(a1, a2, (t - k0) / (k2 - k0));
                let b2 = lerp(a2, a3, (t - k1) / (k3 - k1));
                at.push(lerp(b1, b2, (t - k1) / (k2 - k1)));
            }
        }
        at.push(points[p - 1]);
        let n = at.len();
        let normal: Vec<V3> = (0..n)
            .map(|i| {
                let back =
                    if i == 0 { mesh::sub(at[1], at[0]) } else { mesh::sub(at[i], at[i - 1]) };
                let ahead = if i + 1 == n {
                    mesh::sub(at[i], at[i - 1])
                } else {
                    mesh::sub(at[i + 1], at[i])
                };
                mesh::normalize(mesh::add(mesh::normalize(back), mesh::normalize(ahead)))
            })
            .collect();
        let mut frame = vec![[0.0; 3]; n];
        let mut along = vec![0.0; n];
        let u0 = mesh::sub(up, mesh::scale(normal[0], mesh::dot(up, normal[0])));
        frame[0] = mesh::normalize(u0);
        for i in 1..n {
            let u = carry(frame[i - 1], normal[i - 1], normal[i]);
            frame[i] =
                mesh::normalize(mesh::sub(u, mesh::scale(normal[i], mesh::dot(u, normal[i]))));
            along[i] = along[i - 1] + mesh::length(mesh::sub(at[i], at[i - 1]));
        }
        Path { at, normal, up: frame, along }
    }

    pub fn length(&self) -> f64 {
        *self.along.last().unwrap()
    }

    /// The section plane `s` along the path: its centre, and its up and left (angle 0 and
    /// π/2): the engine's plane through a point there.
    pub fn plane(&self, s: f64) -> (V3, V3, V3) {
        let s = s.clamp(0.0, self.length());
        let i = (0..self.at.len() - 1).rfind(|&i| self.along[i] <= s).unwrap_or(0);
        let (a, b) = (self.at[i], self.at[i + 1]);
        let (n0, n1) = (self.normal[i], self.normal[i + 1]);
        let w = unit(mesh::sub(b, a));
        let len = mesh::length(mesh::sub(b, a));
        let (c0, c1) = (mesh::dot(n0, w), mesh::dot(n1, w));
        let t = (s - self.along[i]).clamp(0.0, len);
        // t = λ L c1 / ((1 − λ) c0 + λ c1), for λ.
        let lam = (t * c0 / (len * c1 - t * c1 + t * c0).max(1e-12)).clamp(0.0, 1.0);
        let (_, u, v) = self.frame(i, lam);
        (mesh::add(a, mesh::scale(w, t)), u, v)
    }

    /// The plane `lam` of the way from piece `i`'s start plane to its end plane: its normal,
    /// and its up and left (angle 0 and π/2).
    fn frame(&self, i: usize, lam: f64) -> (V3, V3, V3) {
        let n = unit(lerp(self.normal[i], self.normal[i + 1], lam));
        let u = lerp(self.up[i], self.up[i + 1], lam);
        let uu = unit(mesh::sub(u, mesh::scale(n, mesh::dot(u, n))));
        (n, uu, mesh::cross(uu, n))
    }

    /// Where `p` is about the path, as the engine's `place`: how far along it (less than 0
    /// before its start, more than its length past its end), how far from it in its plane, and
    /// at what angle.
    pub fn place(&self, p: V3) -> (f64, f64, f64) {
        let n = self.at.len();
        let mut best = (0.0, 0.0, 0.0);
        let mut key = f64::INFINITY;
        for i in 0..n - 1 {
            let (a, b) = (self.at[i], self.at[i + 1]);
            let (n0, n1) = (self.normal[i], self.normal[i + 1]);
            let g0 = mesh::dot(n0, mesh::sub(p, a));
            let g1 = mesh::dot(n1, mesh::sub(b, p));
            let lam = (g0 / (g0 + g1).max(1e-9)).clamp(0.0, 1.0);
            let w = unit(mesh::sub(b, a));
            let (c0, c1) = (mesh::dot(n0, w), mesh::dot(n1, w));
            let len = mesh::length(mesh::sub(b, a));
            let t = (lam * len * c1 / ((1.0 - lam) * c0 + lam * c1).max(1e-6)).clamp(0.0, len);
            let (nn, uu, v) = self.frame(i, lam);
            let q = mesh::sub(p, mesh::add(a, mesh::scale(w, t)));
            let q_in = mesh::sub(q, mesh::scale(nn, mesh::dot(q, nn)));
            let rho = mesh::length(q_in);
            let theta =
                if rho < 1e-6 { 0.0 } else { mesh::dot(q_in, v).atan2(mesh::dot(q_in, uu)) };
            let (first, last) = (i == 0, i + 2 == n);
            let before = if first { g0.min(0.0) } else { 0.0 };
            let past = if last { (-g1).max(0.0) } else { 0.0 };
            let outside =
                if first { 0.0 } else { (-g0).max(0.0) } + if last { 0.0 } else { (-g1).max(0.0) };
            let k = (rho * rho + outside * outside).sqrt();
            if k < key {
                key = k;
                best = (self.along[i] + t + before + past, rho, theta);
            }
        }
        best
    }

    /// The point `r` from the path, `s` along it, at angle `theta`.
    pub fn point(&self, s: f64, theta: f64, r: f64) -> V3 {
        let (c, u, v) = self.plane(s);
        mesh::add(c, mesh::add(mesh::scale(u, r * theta.cos()), mesh::scale(v, r * theta.sin())))
    }
}

/// `points` run on by `overlap` past each end (along the path's direction there), then each end
/// moved along the path to where it's inside the mesh (to 1 mm): the trimmed points, and how much
/// went from each end; `None` if none of the path is inside.
pub fn trimmed(solid: &Solid, points: &[V3], up: V3, overlap: f64) -> Option<(Vec<V3>, f64, f64)> {
    let mut pts = points.to_vec();
    let n = pts.len();
    let start = unit(mesh::sub(pts[1], pts[0]));
    let end = unit(mesh::sub(pts[n - 1], pts[n - 2]));
    pts[0] = mesh::sub(pts[0], mesh::scale(start, overlap));
    pts[n - 1] = mesh::add(pts[n - 1], mesh::scale(end, overlap));
    let path = Path::new(&pts, up);
    let len = path.length();
    let steps = (len / 0.001).ceil().max(1.0) as usize;
    let at = |k: usize| path.plane(len * k as f64 / steps as f64).0;
    let first = (0..=steps).find(|&k| solid.inside(at(k)))?;
    let last = (first..=steps).rev().find(|&k| solid.inside(at(k)))?;
    if last == first {
        return None;
    }
    let step = len / steps as f64;
    pts[0] = at(first);
    pts[n - 1] = at(last);
    Some((pts, first as f64 * step, (steps - last) as f64 * step))
}

// ---- measuring and fitting --------------------------------------------------------------------

/// The radii measured along a path: at `stations` places along it (`s`, evenly from 0 to its
/// length), `angles` each (`radii[i * angles + j]`, metres), and which ran on past `r_max`
/// (held there: free, as a loft's are).
pub struct Measured {
    pub s: Vec<f64>,
    pub angles: usize,
    pub radii: Vec<f64>,
    pub held: Vec<bool>,
}

pub fn measure(solid: &Solid, path: &Path, spacing: f64, angles: usize, r_max: f64) -> Measured {
    let len = path.length();
    let stations = ((len / spacing).ceil() as usize + 1).max(2);
    let s: Vec<f64> = (0..stations).map(|i| len * i as f64 / (stations - 1) as f64).collect();
    let mut radii = Vec::with_capacity(stations * angles);
    let mut held = Vec::with_capacity(stations * angles);
    for &si in &s {
        let (c, u, v) = path.plane(si);
        let inside = solid.inside(c);
        for j in 0..angles {
            let theta = std::f64::consts::TAU * j as f64 / angles as f64;
            let d = mesh::add(mesh::scale(u, theta.cos()), mesh::scale(v, theta.sin()));
            let r = if inside { solid.exit(c, d).unwrap_or(0.0) } else { 0.0 };
            held.push(r > r_max);
            radii.push(r.min(r_max));
        }
    }
    Measured { s, angles, radii, held }
}

impl Measured {
    /// The path's length.
    fn len(&self) -> f64 {
        *self.s.last().unwrap()
    }

    /// The angles of each station's radii.
    fn thetas(&self) -> Vec<f64> {
        let a = self.angles;
        (0..a).map(|j| std::f64::consts::TAU * j as f64 / a as f64).collect()
    }

    /// The measured radius `s` along the path and at angle `theta`: linear between stations
    /// and angles, as a loft's.
    pub fn radius(&self, s: f64, theta: f64) -> f64 {
        let n = self.s.len();
        let len = self.len();
        let ts = (s / len).clamp(0.0, 1.0) * (n - 1) as f64;
        let i0 = (ts as usize).min(n - 2);
        let f = ts - i0 as f64;
        let a = self.angles;
        let js = (theta / std::f64::consts::TAU).rem_euclid(1.0) * a as f64;
        let j0 = (js as usize).min(a - 1);
        let g = js - j0 as f64;
        let j1 = (j0 + 1) % a;
        let r = |i: usize, j: usize| self.radii[i * a + j];
        let near = r(i0, j0) + (r(i0, j1) - r(i0, j0)) * g;
        let far = r(i0 + 1, j0) + (r(i0 + 1, j1) - r(i0 + 1, j0)) * g;
        near + (far - near) * f
    }
}

/// Whether `p` is inside a part as measured (its radii, linear between them; flat ends).
pub fn inside_measured(path: &Path, m: &Measured, p: V3) -> bool {
    let (along, rho, theta) = path.place(p);
    along >= 0.0 && along <= path.length() && rho < m.radius(along, theta)
}

/// Whether `p` is inside a fitted sweep (flat ends).
pub fn inside_fitted(path: &Path, f: &Fitted, p: V3) -> bool {
    let (along, rho, theta) = path.place(p);
    let len = path.length();
    along >= 0.0
        && along <= len
        && rho < section_at(&f.sections, &f.places, along / len).radius(theta)
}

/// What a measured radius is to its part's fit: its own surface (no other part holds the point
/// 0.5 mm inside it), shared (another does: a ray that ran on along it), or held at `r_max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Own,
    Shared,
    Held,
}

/// What a fit gave: the control sections at their places, and how far the fit is from the
/// measured radii (root mean square, and the share within 2 mm).
#[derive(Clone)]
pub struct Fitted {
    pub sections: Vec<Section>,
    pub places: Vec<f64>,
    /// How many bumps each section has.
    pub bumps: usize,
    pub rms: f64,
    pub within_2mm: f64,
}

/// The numbers a fit moves for each section: the named six, then three per bump.
fn per_section(bumps: usize) -> usize {
    6 + 3 * bumps
}

fn unpack(x: &[f64], k: usize, bumps: usize) -> Vec<Section> {
    let p = per_section(bumps);
    (0..k).map(|i| Section::from_params(&x[i * p..(i + 1) * p])).collect()
}

/// Extents from 0.1 mm to `r_max`; squareness from 1 to 8; a bump 0.2 to 1.6 radians wide (a
/// narrower one is a fin, not a form: fitted to rays that ran along another part, they stood
/// out of the hindquarters like blades) and
/// at most half its section's mean extent high (each way): two bumps can't cancel to nothing.
fn hold(x: &mut [f64], bumps: usize, r_max: f64) {
    let p = per_section(bumps);
    for sec in x.chunks_mut(p) {
        for e in &mut sec[..4] {
            *e = e.clamp(0.0001, r_max);
        }
        sec[4] = sec[4].clamp(1.0, 8.0);
        let most = 0.125 * (sec[0] + sec[1] + sec[2] + sec[3]);
        for b in sec[6..].chunks_mut(3) {
            b[1] = b[1].clamp(-most, most);
            b[2] = b[2].clamp(0.2, 1.6);
        }
    }
}

/// How far short of a measured radius a fitted sweep may be where it's held up (`refit`): within
/// the replica's 2 mm. A part short of its own surface by a little isn't a crack (its own inside
/// is behind it); one short by more where nothing else reaches opens one.
pub const SLACK: f64 = 0.002;

/// How much a shared radius counts where the fit is short of it (in full where it's past it:
/// there the sweep would leave the mesh).
const SHARED: f64 = 0.02;

/// A measured radius's weight in a fit, by its kind and how far it is past the fit (`e`): a
/// radius of the part's own surface in full; a shared one in full where the fit passes it and
/// by `SHARED` where it's short; one held, not at all.
fn weight(kind: Kind, e: f64) -> f64 {
    match kind {
        Kind::Own => 1.0,
        Kind::Shared if e < 0.0 => 1.0,
        Kind::Shared => SHARED,
        Kind::Held => 0.0,
    }
}

/// Which of its bump's numbers a section's number `q` is (0 the angle, 1 the height, 2 the
/// width), if it is a bump's.
fn bump_slot(q: usize) -> Option<usize> {
    (q >= 6).then(|| (q - 6) % 3)
}

/// The finite-difference step for a section's number `q`: finer for extents and bumps' heights.
fn diff_step(q: usize) -> f64 {
    if q < 4 || bump_slot(q) == Some(1) { 1e-5 } else { 1e-4 }
}

/// Fits `k` control sections (evenly along the path) with `bumps` bumps each to the measured
/// radii (`kinds`). Each station first, alone: the one with the most of its own surface from a
/// circle (bumps added a time each where it misses most), then each further one from its
/// neighbour's fit, held a little toward it, so a bump follows its feature along the path. Each
/// number of the control sections then by least squares to those stations' (the curve is
/// linear in them, bumps' angles unwrapped along the path); then all together (`rounds`).
/// Fitting each control section to the station nearest it, bumps sorted by angle, mixed
/// unrelated bumps between sections: 56% of a hind leg within 2 mm, where these held 91%.
pub fn fit(m: &Measured, kinds: &[Kind], k: usize, bumps: usize, r_max: f64) -> Fitted {
    let len = m.len();
    let places: Vec<f64> =
        if k == 1 { vec![0.0] } else { (0..k).map(|i| i as f64 / (k - 1) as f64).collect() };
    let a = m.angles;
    let theta = m.thetas();
    let n = m.s.len();
    let row = |i: usize| (&m.radii[i * a..(i + 1) * a], &kinds[i * a..(i + 1) * a]);
    let start =
        (0..n).max_by_key(|&i| row(i).1.iter().filter(|&&x| x == Kind::Own).count()).unwrap_or(0);
    let mut per: Vec<Section> = vec![Section::default(); n];
    let (r0, k0) = row(start);
    per[start] = fit_station(&theta, r0, k0, None, bumps, r_max);
    for i in start + 1..n {
        let (r, ki) = row(i);
        per[i] = fit_station(&theta, r, ki, Some(&per[i - 1]), bumps, r_max);
    }
    for i in (0..start).rev() {
        let (r, ki) = row(i);
        per[i] = fit_station(&theta, r, ki, Some(&per[i + 1]), bumps, r_max);
    }
    // Each number's track along the path (bumps' angles unwrapped), then its control values.
    let p = per_section(bumps);
    let mut tracks: Vec<Vec<f64>> = per.iter().map(|sec| sec.params(bumps)).collect();
    for b in 0..bumps {
        let q = 6 + 3 * b;
        for i in 1..n {
            tracks[i][q] = tracks[i - 1][q] + wrap(tracks[i][q] - tracks[i - 1][q]);
        }
    }
    let basis: Vec<Vec<f64>> = m.s.iter().map(|&s| curve_weights(&places, s / len)).collect();
    let mut x = vec![0.0; k * p];
    for q in 0..p {
        let ys: Vec<f64> = tracks.iter().map(|t| t[q]).collect();
        let c = crate::reference::least_squares(&basis, &ys, 1e-9);
        for (i, v) in c.iter().enumerate() {
            x[i * p + q] = *v;
        }
    }
    let no_floor = vec![false; m.radii.len()];
    let x = rounds(m, kinds, &no_floor, &places, x, bumps, r_max, 3);
    finish(m, kinds, unpack(&x, k, bumps), places, bumps)
}

/// The weights of each control section (at `places`) in the section `f` of the way along:
/// the engine's curve, its control sections past the first and last carried on as lines.
fn curve_weights(places: &[f64], f: f64) -> Vec<f64> {
    let k = places.len();
    let mut out = vec![0.0; k];
    if k == 1 || f <= places[0] {
        out[0] = 1.0;
        return out;
    }
    if f >= places[k - 1] {
        out[k - 1] = 1.0;
        return out;
    }
    let m = span(places, f);
    let x = ((f - places[m]) / (places[m + 1] - places[m])).clamp(0.0, 1.0);
    let w = catmull_rom(x);
    for (d, wd) in w.iter().enumerate() {
        match m as isize + d as isize - 1 {
            -1 => {
                out[0] += 2.0 * wd;
                out[1] -= wd;
            }
            c if c as usize > k - 1 => {
                out[k - 1] += 2.0 * wd;
                out[k - 2] -= wd;
            }
            c => out[c as usize] += wd,
        }
    }
    out
}

/// How hard a station's fit is held toward its neighbour's: per number, a miss of this many of
/// a section's mean extent counts as much as one radius missed by that extent.
const PULL: f64 = 0.3;

/// One station's section, fitted to its radii (`kinds`): from a circle at the median of its own
/// radii, bumps added one at a time where it misses most; or from `from`, held toward it
/// (`PULL`). Levenberg and Marquardt's method, its numbers held within `hold`'s bounds.
fn fit_station(
    theta: &[f64],
    r: &[f64],
    kinds: &[Kind],
    from: Option<&Section>,
    bumps: usize,
    r_max: f64,
) -> Section {
    let p = per_section(bumps);
    let weigh = |sec: &Section| -> Vec<f64> {
        let outline = sec.outline();
        theta
            .iter()
            .zip(r)
            .zip(kinds)
            .map(|((&t, &y), &k)| weight(k, y - outline.radius(t)))
            .collect()
    };
    match from {
        Some(prev) => {
            let x0 = prev.params(bumps);
            let size = 0.25 * (prev.up + prev.left + prev.down + prev.right).max(0.004);
            // A number's natural size: extents and heights in metres, squareness in units,
            // angles at the section's size.
            let unit: Vec<f64> = (0..p)
                .map(|q| match q {
                    4 => 0.03 / size,
                    5 => 1.0,
                    q if bump_slot(q).is_some_and(|b| b != 1) => 1.0,
                    _ => 1.0 / size,
                })
                .collect();
            let w = weigh(prev);
            let x = station_lm(theta, r, &w, x0.clone(), bumps, r_max, |x| {
                (0..p)
                    .map(|q| {
                        let d =
                            if bump_slot(q) == Some(0) { wrap(x[q] - x0[q]) } else { x[q] - x0[q] };
                        PULL * size * d * unit[q]
                    })
                    .collect()
            });
            Section::from_params(&x)
        }
        None => {
            let mut own: Vec<f64> = r
                .iter()
                .zip(kinds)
                .filter(|(y, k)| **k == Kind::Own && **y > 0.0)
                .map(|(y, _)| *y)
                .collect();
            own.sort_by(f64::total_cmp);
            let med = if own.is_empty() { 0.02 } else { own[own.len() / 2] }.min(r_max);
            let mut sec = Section::round(med);
            let mut x = sec.params(0);
            x = station_lm(theta, r, &weigh(&sec), x, 0, r_max, |_| Vec::new());
            for b in 0..bumps {
                sec = Section::from_params(&x);
                let outline = sec.outline();
                let (mut worst, mut at, mut e_at) = (0.0, 0.0, 0.0);
                for ((&t, &y), &k) in theta.iter().zip(r).zip(kinds) {
                    let e = y - outline.radius(t);
                    let counted = match k {
                        Kind::Own => e.abs(),
                        Kind::Shared => (-e).max(0.0),
                        Kind::Held => 0.0,
                    };
                    if counted > worst {
                        worst = counted;
                        at = t;
                        e_at = e;
                    }
                }
                x.extend([at, e_at.clamp(-0.04, 0.04), 0.4]);
                x = station_lm(
                    theta,
                    r,
                    &weigh(&Section::from_params(&x)),
                    x,
                    b + 1,
                    r_max,
                    |_| Vec::new(),
                );
            }
            let mut out = Section::from_params(&x);
            for b in bumps.min(4)..4 {
                out.bumps[b] = [0.0; 3];
            }
            out
        }
    }
}

/// Minimizes one station's weighted misses (`w`) and the terms `extra` gives, over `x` (a
/// section's numbers, `bumps` bumps).
fn station_lm(
    theta: &[f64],
    r: &[f64],
    w: &[f64],
    x: Vec<f64>,
    bumps: usize,
    r_max: f64,
    extra: impl Fn(&[f64]) -> Vec<f64>,
) -> Vec<f64> {
    let p = x.len();
    let res = |x: &[f64]| -> Vec<f64> {
        let sec = Section::from_params(x);
        let outline = sec.outline();
        let mut e: Vec<f64> = theta
            .iter()
            .zip(r)
            .zip(w)
            .map(|((&t, &y), &wi)| wi.sqrt() * (outline.radius(t) - y))
            .collect();
        e.extend(extra(x));
        e
    };
    let cost = |x: &[f64]| res(x).iter().map(|e| e * e).sum::<f64>();
    let normal = |x: &[f64]| {
        let e0 = res(x);
        let mut jac = vec![vec![0.0; e0.len()]; p];
        for q in 0..p {
            let step = diff_step(q);
            let mut y = x.to_vec();
            y[q] += step;
            let e1 = res(&y);
            for (jq, (a, b)) in jac[q].iter_mut().zip(e1.iter().zip(&e0)) {
                *jq = (a - b) / step;
            }
        }
        let mut h = Banded::new(p, p);
        let mut g = vec![0.0; p];
        for q in 0..p {
            g[q] = jac[q].iter().zip(&e0).map(|(a, b)| a * b).sum();
            for t in 0..=q {
                h.add(q, t, jac[q].iter().zip(&jac[t]).map(|(a, b)| a * b).sum());
            }
        }
        (h, g)
    };
    let stop = Stop { steps: 100, gain: 1e-6, damping: 1e7 };
    levenberg_marquardt(x, bumps, r_max, stop, cost, normal)
}

/// When `levenberg_marquardt` stops: after `steps` steps, after a step that lowers the cost by
/// less than `gain` of it, or when the damping passes `damping`.
struct Stop {
    steps: usize,
    gain: f64,
    damping: f64,
}

/// Levenberg and Marquardt's method: minimizes `cost` over `x` (sections' numbers, `bumps`
/// bumps each, held within `hold`'s bounds), each step from `normal`'s JᵀJ and Jᵀe at `x`,
/// damped.
fn levenberg_marquardt(
    mut x: Vec<f64>,
    bumps: usize,
    r_max: f64,
    stop: Stop,
    cost: impl Fn(&[f64]) -> f64,
    normal: impl Fn(&[f64]) -> (Banded, Vec<f64>),
) -> Vec<f64> {
    hold(&mut x, bumps, r_max);
    let mut c = cost(&x);
    let mut lambda = 1e-3;
    for _ in 0..stop.steps {
        let (mut h, g) = normal(&x);
        for q in 0..x.len() {
            let d = h.at(q, q);
            h.add(q, q, lambda * d + 1e-12);
        }
        let step = h.solve(g.iter().map(|v| -v).collect());
        let mut y: Vec<f64> = x.iter().zip(&step).map(|(a, b)| a + b).collect();
        hold(&mut y, bumps, r_max);
        let cy = cost(&y);
        if cy < c {
            let gain = (c - cy) / c.max(1e-30);
            x = y;
            c = cy;
            lambda = (lambda / 3.0).max(1e-9);
            if gain < stop.gain {
                break;
            }
        } else {
            lambda *= 5.0;
            if lambda > stop.damping {
                break;
            }
        }
    }
    x
}

/// A fit again from `previous`, its radii in `floor` held up to within `SLACK` of their length:
/// each one the fit is shorter than that pressed on harder each round, until none is.
pub fn refit(
    m: &Measured,
    kinds: &[Kind],
    floor: &[bool],
    previous: &Fitted,
    r_max: f64,
) -> Fitted {
    let used = previous.bumps;
    let x: Vec<f64> = previous.sections.iter().flat_map(|s| s.params(used)).collect();
    let x = rounds(m, kinds, floor, &previous.places, x, used, r_max, 12);
    finish(m, kinds, unpack(&x, previous.sections.len(), used), previous.places.clone(), used)
}

fn finish(
    m: &Measured,
    kinds: &[Kind],
    sections: Vec<Section>,
    places: Vec<f64>,
    bumps: usize,
) -> Fitted {
    let len = m.len();
    let a = m.angles;
    let theta = m.thetas();
    let (mut sq, mut n, mut within) = (0.0, 0usize, 0usize);
    for (i, &s) in m.s.iter().enumerate() {
        let sec = section_at(&sections, &places, s / len);
        let outline = sec.outline();
        for j in 0..a {
            if kinds[i * a + j] != Kind::Own {
                continue;
            }
            let e = m.radii[i * a + j] - outline.radius(theta[j]);
            sq += e * e;
            n += 1;
            within += usize::from(e.abs() <= 0.002);
        }
    }
    Fitted {
        sections,
        places,
        bumps,
        rms: (sq / n.max(1) as f64).sqrt(),
        within_2mm: within as f64 / n.max(1) as f64,
    }
}

/// `count` rounds of `sweep_lm`, the radii weighted from each round's fit: a radius of the
/// part's own surface in full; a shared one in full where the fit passes it (it would leave the
/// mesh) and by `SHARED` where it's short; one held, not at all; and one in `floor` the fit is
/// short of, harder each round (stopping early once none is short).
#[allow(clippy::too_many_arguments)]
fn rounds(
    m: &Measured,
    kinds: &[Kind],
    floor: &[bool],
    places: &[f64],
    mut x: Vec<f64>,
    bumps: usize,
    r_max: f64,
    count: usize,
) -> Vec<f64> {
    let (a, len, theta) = (m.angles, m.len(), m.thetas());
    let k = places.len();
    let mut press = vec![0.0f64; m.radii.len()];
    for round in 0..count {
        let secs = unpack(&x, k, bumps);
        let mut w = vec![0.0; m.radii.len()];
        let mut short = false;
        for (i, &s) in m.s.iter().enumerate() {
            let sec = section_at(&secs, places, s / len);
            let outline = sec.outline();
            for (j, &t) in theta.iter().enumerate() {
                let n = i * a + j;
                let e = m.radii[n] - outline.radius(t);
                if floor[n] && e > SLACK + 0.00005 {
                    press[n] = (press[n] * 10.0).max(10.0);
                    short = true;
                }
                w[n] = weight(kinds[n], e);
            }
        }
        if round > 2 && !short && floor.iter().any(|&f| f) {
            break;
        }
        x = sweep_lm(m, places, x, bumps, &w, &press, r_max);
    }
    x
}

/// Minimizes the weighted squared misses of the measured radii (`w`, 0 for those held), and
/// how far the fit is short of each held up (`press`, less `SLACK`), over `x` (the control
/// sections' numbers, `bumps` bumps each).
fn sweep_lm(
    m: &Measured,
    places: &[f64],
    x: Vec<f64>,
    bumps: usize,
    w: &[f64],
    press: &[f64],
    r_max: f64,
) -> Vec<f64> {
    let (a, len, theta) = (m.angles, m.len(), m.thetas());
    let k = places.len();
    let p = per_section(bumps);
    let n = x.len();
    let support: Vec<Vec<usize>> =
        m.s.iter()
            .map(|&s| {
                if k == 1 {
                    return vec![0];
                }
                let mm = span(places, s / len);
                (mm.saturating_sub(1)..(mm + 3).min(k)).collect()
            })
            .collect();
    // Station i's radii's weighted misses, of the sections `secs`; then, for each held up
    // (`press`), how far the fit is shorter than it less `SLACK` (0 where it isn't).
    let misses = |secs: &[Section], i: usize| -> Vec<f64> {
        let sec = section_at(secs, places, m.s[i] / len);
        let outline = sec.outline();
        let f: Vec<f64> = (0..a).map(|j| outline.radius(theta[j])).collect();
        let mut e: Vec<f64> =
            (0..a).map(|j| w[i * a + j].sqrt() * (f[j] - m.radii[i * a + j])).collect();
        e.extend(
            (0..a)
                .map(|j| press[i * a + j].sqrt() * (f[j] - (m.radii[i * a + j] - SLACK)).min(0.0)),
        );
        e
    };
    let cost = |x: &[f64]| -> f64 {
        let secs = unpack(x, k, bumps);
        (0..m.s.len()).map(|i| misses(&secs, i).iter().map(|e| e * e).sum::<f64>()).sum::<f64>()
    };
    let normal = |x: &[f64]| {
        let mut h = Banded::new(n, 4 * p);
        let mut g = vec![0.0; n];
        let mut secs = unpack(x, k, bumps);
        for (i, sup) in support.iter().enumerate() {
            let e0 = misses(&secs, i);
            let cols: Vec<usize> = sup.iter().flat_map(|&s| s * p..(s + 1) * p).collect();
            let mut jac: Vec<Vec<f64>> = Vec::with_capacity(cols.len());
            for &q in &cols {
                // Number q stepped: only its section changes.
                let step = diff_step(q % p);
                let s = q / p;
                let mut y = x[s * p..(s + 1) * p].to_vec();
                y[q % p] += step;
                let kept = std::mem::replace(&mut secs[s], Section::from_params(&y));
                let e1 = misses(&secs, i);
                secs[s] = kept;
                jac.push(e1.iter().zip(&e0).map(|(a, b)| (a - b) / step).collect());
            }
            for (ci, &q) in cols.iter().enumerate() {
                g[q] += jac[ci].iter().zip(&e0).map(|(j, e)| j * e).sum::<f64>();
                for (cj, &r) in cols.iter().enumerate() {
                    if r <= q {
                        let v: f64 = jac[ci].iter().zip(&jac[cj]).map(|(a, b)| a * b).sum();
                        h.add(q, r, v);
                    }
                }
            }
        }
        (h, g)
    };
    let stop = Stop { steps: 60, gain: 1e-5, damping: 1e6 };
    levenberg_marquardt(x, bumps, r_max, stop, cost, normal)
}
