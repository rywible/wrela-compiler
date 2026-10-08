//! Triangle meshes for `wrela reference`: OBJ and binary STL files, placed in a creature's
//! frame, and a bounding volume hierarchy for the questions a replica asks of a sculpt: where a
//! ray first meets the surface, whether a point is inside, and how far the nearest surface is.
//! In `f64`, so a sculpt's thin parts don't fall between rounding errors.

use std::path::Path;

pub type V3 = [f64; 3];

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub fn length(a: V3) -> f64 {
    dot(a, a).sqrt()
}

pub fn normalize(a: V3) -> V3 {
    scale(a, 1.0 / length(a))
}

/// A triangle soup with shared vertices.
pub struct Mesh {
    pub vertices: Vec<V3>,
    pub triangles: Vec<[u32; 3]>,
}

/// Reads an OBJ (its `v` and `f` lines; polygons are fanned into triangles) or a binary STL
/// (vertices merged where their coordinates are equal), by the file's extension.
pub fn load(path: &Path) -> Result<Mesh, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "obj" => parse_obj(&String::from_utf8_lossy(&bytes)),
        "stl" => parse_stl(&bytes),
        _ => Err(format!("{}: a mesh is an .obj or .stl file", path.display())),
    }
}

pub fn parse_obj(text: &str) -> Result<Mesh, String> {
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("v") => {
                let mut p = [0.0; 3];
                for c in &mut p {
                    *c = words
                        .next()
                        .and_then(|w| w.parse().ok())
                        .ok_or_else(|| format!("line {}: a vertex has three numbers", n + 1))?;
                }
                vertices.push(p);
            }
            Some("f") => {
                let mut corners = Vec::new();
                for w in words {
                    let i: i64 =
                        w.split('/').next().and_then(|s| s.parse().ok()).ok_or_else(|| {
                            format!("line {}: a face's corner isn't a number", n + 1)
                        })?;
                    // OBJ counts from 1; a negative index counts back from the last vertex.
                    let i = if i < 0 { vertices.len() as i64 + i } else { i - 1 };
                    if i < 0 || i as usize >= vertices.len() {
                        return Err(format!(
                            "line {}: a face names a vertex that isn't there",
                            n + 1
                        ));
                    }
                    corners.push(i as u32);
                }
                for k in 1..corners.len().saturating_sub(1) {
                    triangles.push([corners[0], corners[k], corners[k + 1]]);
                }
            }
            _ => {}
        }
    }
    if triangles.is_empty() {
        return Err("the OBJ has no faces".to_string());
    }
    Ok(Mesh { vertices, triangles })
}

pub fn parse_stl(bytes: &[u8]) -> Result<Mesh, String> {
    if bytes.len() < 84 {
        return Err("the STL is too short to be a binary STL".to_string());
    }
    let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    if bytes.len() != 84 + 50 * n {
        return Err("the STL isn't a binary STL (ASCII STL isn't read)".to_string());
    }
    if n == 0 {
        return Err("the STL has no triangles".to_string());
    }
    let mut index = std::collections::HashMap::new();
    let mut vertices = Vec::new();
    let mut triangles = Vec::with_capacity(n);
    for t in 0..n {
        let base = 84 + 50 * t + 12;
        let mut tri = [0u32; 3];
        for (c, corner) in tri.iter_mut().enumerate() {
            let mut key = [0u32; 3];
            let mut p = [0.0; 3];
            for k in 0..3 {
                let o = base + 12 * c + 4 * k;
                let bits = u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
                key[k] = bits;
                p[k] = f64::from(f32::from_bits(bits));
            }
            *corner = *index.entry(key).or_insert_with(|| {
                vertices.push(p);
                (vertices.len() - 1) as u32
            });
        }
        triangles.push(tri);
    }
    Ok(Mesh { vertices, triangles })
}

impl Mesh {
    /// The mesh's pieces (triangles joined by shared vertices), largest first: each piece's
    /// triangles.
    pub fn pieces(&self) -> Vec<Vec<u32>> {
        let mut parent: Vec<u32> = (0..self.vertices.len() as u32).collect();
        fn find(parent: &mut [u32], mut x: u32) -> u32 {
            while parent[x as usize] != x {
                parent[x as usize] = parent[parent[x as usize] as usize];
                x = parent[x as usize];
            }
            x
        }
        for t in &self.triangles {
            let r = find(&mut parent, t[0]);
            for &v in &t[1..] {
                let s = find(&mut parent, v);
                parent[s as usize] = r;
            }
        }
        let mut by_root: std::collections::HashMap<u32, Vec<u32>> = Default::default();
        for (i, t) in self.triangles.iter().enumerate() {
            by_root.entry(find(&mut parent, t[0])).or_default().push(i as u32);
        }
        let mut pieces: Vec<Vec<u32>> = by_root.into_values().collect();
        pieces.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
        pieces
    }

    /// The triangles' corners, each triangle's three points.
    pub fn corners(&self, triangles: &[u32]) -> Vec<[V3; 3]> {
        triangles
            .iter()
            .map(|&t| {
                let [a, b, c] = self.triangles[t as usize];
                [self.vertices[a as usize], self.vertices[b as usize], self.vertices[c as usize]]
            })
            .collect()
    }
}

/// How a mesh's coordinates map to a creature's frame (+y up, facing +z, metres): each frame
/// axis is a file axis, maybe negated (`axes`, as "x,y,z" or "-z,y,x"), after moving `origin`
/// (in the file's coordinates) to the frame's origin, times `metres_per_unit`.
#[derive(Clone, Copy, Debug)]
pub struct Registration {
    pub axes: [(usize, f64); 3],
    pub origin: V3,
    pub metres_per_unit: f64,
}

impl Registration {
    pub fn parse_axes(text: &str) -> Result<[(usize, f64); 3], String> {
        let mut out = [(0, 1.0); 3];
        let words: Vec<&str> = text.split(',').map(str::trim).collect();
        if words.len() != 3 {
            return Err(format!("`axes` is three file axes, as \"x,y,z\": not `{text}`"));
        }
        for (k, w) in words.iter().enumerate() {
            let (sign, name) = match w.strip_prefix('-') {
                Some(n) => (-1.0, n),
                None => (1.0, *w),
            };
            let axis = match name {
                "x" => 0,
                "y" => 1,
                "z" => 2,
                _ => return Err(format!("`axes`: `{w}` isn't x, y or z, maybe negated")),
            };
            out[k] = (axis, sign);
        }
        let mut seen = [false; 3];
        for (a, _) in out {
            seen[a] = true;
        }
        if seen.contains(&false) {
            return Err(format!("`axes` names each file axis once: not `{text}`"));
        }
        Ok(out)
    }

    pub fn place(&self, p: V3) -> V3 {
        let q = sub(p, self.origin);
        let mut out = [0.0; 3];
        for (k, (axis, sign)) in self.axes.iter().enumerate() {
            out[k] = q[*axis] * sign * self.metres_per_unit;
        }
        out
    }

    /// Whether the axes keep handedness (an odd number of swaps or negations mirrors the mesh,
    /// which turns its triangles inside out).
    pub fn keeps_handedness(&self) -> bool {
        let m = |k: usize| {
            let mut v = [0.0; 3];
            v[self.axes[k].0] = self.axes[k].1;
            v
        };
        dot(cross(m(0), m(1)), m(2)) > 0.0
    }
}

/// Triangles placed in a creature's frame, wound so their normals point out, and a bounding
/// volume hierarchy over them.
pub struct Solid {
    pub triangles: Vec<[V3; 3]>,
    nodes: Vec<Node>,
    order: Vec<u32>,
}

#[derive(Clone, Copy)]
struct Node {
    lo: V3,
    hi: V3,
    /// A leaf's triangles: `count` (more than 0) from `first` in `order`. An inner node has a
    /// count of 0 and two children.
    first: u32,
    count: u32,
    children: [u32; 2],
}

const LEAF: usize = 4;

impl Solid {
    /// The triangles, wound outward: if their signed volume is negative, every one is turned.
    pub fn new(mut triangles: Vec<[V3; 3]>) -> Solid {
        let volume: f64 = triangles.iter().map(|t| dot(t[0], cross(t[1], t[2]))).sum();
        if volume < 0.0 {
            for t in &mut triangles {
                t.swap(1, 2);
            }
        }
        let mut s =
            Solid { order: (0..triangles.len() as u32).collect(), triangles, nodes: Vec::new() };
        let n = s.order.len();
        s.build(0, n);
        s
    }

    pub fn bounds(&self) -> (V3, V3) {
        (self.nodes[0].lo, self.nodes[0].hi)
    }

    fn build(&mut self, start: usize, end: usize) -> u32 {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        let (mut clo, mut chi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for &t in &self.order[start..end] {
            let tri = self.triangles[t as usize];
            for p in tri {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            let c = centroid(&tri);
            for k in 0..3 {
                clo[k] = clo[k].min(c[k]);
                chi[k] = chi[k].max(c[k]);
            }
        }
        let index = self.nodes.len() as u32;
        self.nodes.push(Node {
            lo,
            hi,
            first: start as u32,
            count: (end - start) as u32,
            children: [0; 2],
        });
        if end - start <= LEAF {
            return index;
        }
        let axis =
            (0..3).max_by(|&a, &b| (chi[a] - clo[a]).total_cmp(&(chi[b] - clo[b]))).unwrap_or(0);
        let tris = &self.triangles;
        self.order[start..end].sort_by(|&a, &b| {
            centroid(&tris[a as usize])[axis].total_cmp(&centroid(&tris[b as usize])[axis])
        });
        let mid = (start + end) / 2;
        let left = self.build(start, mid);
        let right = self.build(mid, end);
        let node = &mut self.nodes[index as usize];
        node.count = 0;
        node.children = [left, right];
        index
    }

    /// Every crossing of the surface by the ray from `o` along `d` (unit length): its distance,
    /// and +1 where the ray leaves (meets a triangle's outward side from inside) or -1 where it
    /// enters. Nearest first; a crossing through an edge or a vertex counted once.
    pub fn crossings(&self, o: V3, d: V3) -> Vec<(f64, i32)> {
        let inv = [1.0 / d[0], 1.0 / d[1], 1.0 / d[2]];
        let mut out: Vec<(f64, i32, u32)> = Vec::new();
        let mut stack = vec![0u32];
        while let Some(i) = stack.pop() {
            let node = self.nodes[i as usize];
            if !slab(o, inv, node.lo, node.hi) {
                continue;
            }
            if node.count > 0 {
                for &t in &self.order[node.first as usize..(node.first + node.count) as usize] {
                    let tri = &self.triangles[t as usize];
                    if let Some(h) = ray_triangle(o, d, tri).filter(|&h| h > 0.0) {
                        let n = cross(sub(tri[1], tri[0]), sub(tri[2], tri[0]));
                        out.push((h, if dot(n, d) > 0.0 { 1 } else { -1 }, t));
                    }
                }
            } else {
                stack.extend(node.children);
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        // A ray through an edge or a vertex meets each triangle there: one crossing, where the
        // triangles share a corner. (One that grazes an edge between a face turned towards it
        // and one turned away crosses both ways, and those stay: they cancel. Two surfaces that
        // cross each other where the ray does are two crossings.)
        let mut merged: Vec<(f64, i32, u32)> = Vec::with_capacity(out.len());
        for c in out {
            let same = merged
                .iter()
                .rev()
                .take_while(|m| (c.0 - m.0).abs() <= 1e-9 * m.0.max(1.0))
                .any(|m| m.1 == c.1 && self.share_a_corner(m.2, c.2));
            if !same {
                merged.push(c);
            }
        }
        merged.into_iter().map(|c| (c.0, c.1)).collect()
    }

    fn share_a_corner(&self, a: u32, b: u32) -> bool {
        let (a, b) = (&self.triangles[a as usize], &self.triangles[b as usize]);
        a.iter().any(|p| b.contains(p))
    }

    /// The winding number at `o`: how many times the surface wraps it (1 inside, 0 outside,
    /// 2 where two parts of a sculpt that cross each other overlap): its exits along a ray,
    /// less its entries.
    pub fn winding(&self, o: V3, d: V3) -> i32 {
        self.crossings(o, d).iter().map(|c| c.1).sum()
    }

    /// Whether `p` is inside: a winding number of at least 1, the majority of three rays in
    /// unrelated directions (one may graze an edge). Where a sculpt crosses itself, parity
    /// would call the overlap outside; the winding number doesn't.
    pub fn inside(&self, p: V3) -> bool {
        const DIRECTIONS: [V3; 3] =
            [[0.5773, 0.5774, 0.5773], [-0.6247, 0.2316, 0.7458], [0.1513, -0.8838, 0.4427]];
        let votes = DIRECTIONS.iter().filter(|&&d| self.winding(p, normalize(d)) >= 1).count();
        votes >= 2
    }

    /// How far the ray from `o` along `d` (unit length) goes before it's outside: where the
    /// winding number first falls to 0, past any surface inside the solid (where a sculpt
    /// crosses itself). `None` if `o` is outside.
    pub fn exit(&self, o: V3, d: V3) -> Option<f64> {
        let hits = self.crossings(o, d);
        let mut w: i32 = hits.iter().map(|c| c.1).sum();
        if w < 1 {
            return None;
        }
        for (t, sign) in hits {
            w -= sign;
            if w < 1 {
                return Some(t);
            }
        }
        None
    }

    /// The distance from `p` to the nearest point of the surface.
    pub fn nearest(&self, p: V3) -> f64 {
        let mut best = f64::MAX;
        let mut stack = vec![0u32];
        while let Some(i) = stack.pop() {
            let node = self.nodes[i as usize];
            if box_distance(p, node.lo, node.hi) >= best {
                continue;
            }
            if node.count > 0 {
                for &t in &self.order[node.first as usize..(node.first + node.count) as usize] {
                    best = best
                        .min(length(sub(p, closest_on_triangle(p, &self.triangles[t as usize]))));
                }
            } else {
                stack.extend(node.children);
            }
        }
        best
    }
}

fn centroid(t: &[V3; 3]) -> V3 {
    scale(add(add(t[0], t[1]), t[2]), 1.0 / 3.0)
}

/// Whether the ray meets the box.
fn slab(o: V3, inv: V3, lo: V3, hi: V3) -> bool {
    let (mut t0, mut t1) = (0.0f64, f64::MAX);
    for k in 0..3 {
        if inv[k].is_infinite() {
            // Parallel to these faces (0 × ∞ would be NaN): inside the slab or never in it.
            if o[k] < lo[k] || o[k] > hi[k] {
                return false;
            }
            continue;
        }
        let a = (lo[k] - o[k]) * inv[k];
        let b = (hi[k] - o[k]) * inv[k];
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return false;
        }
    }
    true
}

fn box_distance(p: V3, lo: V3, hi: V3) -> f64 {
    let mut s = 0.0;
    for k in 0..3 {
        let d = (lo[k] - p[k]).max(p[k] - hi[k]).max(0.0);
        s += d * d;
    }
    s.sqrt()
}

/// Möller–Trumbore: the distance along the ray to the triangle, either side.
fn ray_triangle(o: V3, d: V3, t: &[V3; 3]) -> Option<f64> {
    let e1 = sub(t[1], t[0]);
    let e2 = sub(t[2], t[0]);
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-18 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(o, t[0]);
    let u = dot(s, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(d, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some(dot(e2, q) * inv)
}

/// The point of triangle `t` nearest `p` (Ericson, Real-Time Collision Detection 5.1.5).
fn closest_on_triangle(p: V3, t: &[V3; 3]) -> V3 {
    let [a, b, c] = *t;
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return add(a, scale(ab, d1 / (d1 - d3)));
    }
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return add(a, scale(ac, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return add(b, scale(sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denom = 1.0 / (va + vb + vc);
    add(a, add(scale(ab, vb * denom), scale(ac, vc * denom)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A sphere of radius `r` at `c`: `rings` bands of `segments` quads, poles fanned, wound
    /// outward, as OBJ text.
    pub(crate) fn sphere_obj(c: V3, r: f64, rings: usize, segments: usize) -> String {
        let mut s = String::new();
        let at = |i: usize, j: usize| {
            let th = std::f64::consts::PI * i as f64 / rings as f64;
            let ph = std::f64::consts::TAU * j as f64 / segments as f64;
            [c[0] + r * th.sin() * ph.cos(), c[1] + r * th.cos(), c[2] + r * th.sin() * ph.sin()]
        };
        s.push_str(&format!("v {} {} {}\n", c[0], c[1] + r, c[2]));
        for i in 1..rings {
            for j in 0..segments {
                let p = at(i, j);
                s.push_str(&format!("v {} {} {}\n", p[0], p[1], p[2]));
            }
        }
        s.push_str(&format!("v {} {} {}\n", c[0], c[1] - r, c[2]));
        let ring = |i: usize, j: usize| 2 + (i - 1) * segments + j % segments;
        let bottom = 2 + (rings - 1) * segments;
        for j in 0..segments {
            s.push_str(&format!("f 1 {} {}\n", ring(1, j + 1), ring(1, j)));
        }
        for i in 1..rings - 1 {
            for j in 0..segments {
                s.push_str(&format!(
                    "f {} {} {} {}\n",
                    ring(i, j),
                    ring(i, j + 1),
                    ring(i + 1, j + 1),
                    ring(i + 1, j)
                ));
            }
        }
        for j in 0..segments {
            s.push_str(&format!(
                "f {} {} {}\n",
                ring(rings - 1, j),
                ring(rings - 1, j + 1),
                bottom
            ));
        }
        s
    }

    fn solid(text: &str) -> Solid {
        let m = parse_obj(text).expect("parse");
        let all: Vec<u32> = (0..m.triangles.len() as u32).collect();
        Solid::new(m.corners(&all))
    }

    #[test]
    fn obj_faces_are_fanned_and_indices_count_from_1_or_back() {
        let m = parse_obj("v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1/1/1 2/2/2 3 4\nf -1 -2 -3\n")
            .unwrap();
        assert_eq!(m.triangles, vec![[0, 1, 2], [0, 2, 3], [3, 2, 1]]);
        assert!(parse_obj("v 0 0 0\nf 1 2 3\n").is_err());
    }

    #[test]
    fn a_binary_stl_merges_equal_corners() {
        let tri = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| {
            let mut out = vec![0u8; 12];
            for p in [a, b, c] {
                for x in p {
                    out.extend(x.to_le_bytes());
                }
            }
            out.extend([0u8, 0]);
            out
        };
        let mut bytes = vec![0u8; 80];
        bytes.extend(2u32.to_le_bytes());
        bytes.extend(tri([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]));
        bytes.extend(tri([1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]));
        let m = parse_stl(&bytes).unwrap();
        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.triangles.len(), 2);
        assert!(parse_stl(&bytes[..100]).is_err());
        // A header that counts no triangles is no mesh, as an OBJ without faces isn't.
        let mut empty = vec![0u8; 80];
        empty.extend(0u32.to_le_bytes());
        assert!(parse_stl(&empty).is_err_and(|e| e.contains("no triangles")));
    }

    #[test]
    fn axes_name_each_file_axis_once_and_keep_handedness() {
        assert!(Registration::parse_axes("x,y").is_err());
        assert!(Registration::parse_axes("x,x,z").is_err());
        let r = Registration {
            axes: Registration::parse_axes("-z, y, x").unwrap(),
            origin: [1.0, 0.0, 0.0],
            metres_per_unit: 0.5,
        };
        assert!(r.keeps_handedness());
        assert_eq!(r.place([1.0, 2.0, 4.0]), [-2.0, 1.0, 0.0]);
        let mirrored = Registration { axes: Registration::parse_axes("-x,y,z").unwrap(), ..r };
        assert!(!mirrored.keeps_handedness());
    }

    #[test]
    fn a_sphere_answers_inside_exit_and_nearest() {
        let s = solid(&sphere_obj([0.0; 3], 1.0, 24, 48));
        assert!(s.inside([0.1, 0.2, -0.1]));
        assert!(!s.inside([1.2, 0.0, 0.0]));
        // The tessellation is within 1 − cos(π / 48) of the sphere: under 0.3%.
        for d in [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], normalize([1.0, 2.0, 3.0])] {
            let e = s.exit([0.0; 3], d).unwrap();
            assert!((e - 1.0).abs() < 0.01, "exit {e}");
        }
        assert!(s.exit([2.0, 0.0, 0.0], [1.0, 0.0, 0.0]).is_none());
        assert!((s.nearest([2.0, 0.0, 0.0]) - 1.0).abs() < 0.01);
    }

    /// Two spheres that cross each other, as a sculpt's parts can: the overlap is inside (the
    /// winding number is 2 there, where parity would say outside), and a ray from it leaves at
    /// the outer surface, past the inner one.
    #[test]
    fn where_a_mesh_crosses_itself_the_overlap_is_inside() {
        let text = sphere_obj([0.0; 3], 1.0, 16, 32);
        let mut m = parse_obj(&text).unwrap();
        let other = parse_obj(&sphere_obj([1.0, 0.0, 0.0], 1.0, 16, 32)).unwrap();
        let n = m.vertices.len() as u32;
        m.vertices.extend(other.vertices);
        m.triangles.extend(other.triangles.iter().map(|t| [t[0] + n, t[1] + n, t[2] + n]));
        let all: Vec<u32> = (0..m.triangles.len() as u32).collect();
        let s = Solid::new(m.corners(&all));
        let p = [0.5, 0.0, 0.0];
        assert_eq!(s.winding(p, [0.0, 1.0, 0.0]), 2);
        assert!(s.inside(p));
        let e = s.exit(p, [1.0, 0.0, 0.0]).unwrap();
        assert!((e - 1.5).abs() < 0.02, "exit {e}");
    }

    /// The hierarchy finds every crossing brute force finds.
    #[test]
    fn crossings_agree_with_brute_force() {
        let s = solid(&sphere_obj([0.2, -0.1, 0.3], 0.8, 12, 24));
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        };
        for _ in 0..200 {
            let o = [rnd() * 1.5, rnd() * 1.5, rnd() * 1.5];
            let d = normalize([rnd(), rnd(), rnd()]);
            let fast = s.crossings(o, d).len();
            let brute = s
                .triangles
                .iter()
                .filter(|t| ray_triangle(o, d, t).is_some_and(|h| h > 0.0))
                .count();
            assert_eq!(fast, brute);
        }
    }
}
