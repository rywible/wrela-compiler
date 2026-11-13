//! Running the interpreter on real inputs: [`Given`], inputs held as bytes and values, and
//! textures read as a GPU reads them (`texture`): a texel load exactly, a sample nearest or
//! linear, with mip levels, repeating or clamped, of 2D, 2D array, 3D and cube textures, and
//! comparison samples. Anisotropic filtering is computed as trilinear, and marked approximate.

use super::{Error, Inputs, Result, SamplerDesc, Texels, V, zero_like};
use crate::*;
use std::collections::HashMap;

/// Inputs given in full: each binding's bytes, textures and samplers, each builtin's value, the
/// varyings, and (for a fragment's block of four) each derivative call's answer.
#[derive(Clone, Debug, Default)]
pub struct Given {
    pub bytes: HashMap<u32, Vec<u8>>,
    pub textures: HashMap<u32, Texels>,
    pub samplers: HashMap<u32, SamplerDesc>,
    pub builtins: Vec<(BuiltinInput, V)>,
    pub varyings: Option<V>,
    /// Each derivative call's answer, by its place in the invocation's sequence of them (0
    /// past the end); and each call's operand, recorded as the invocation runs.
    pub answers: Vec<V>,
    pub operands: Vec<V>,
    /// Each derivative call's operation, as the invocation makes them.
    pub ops: Vec<Builtin>,
}

impl Inputs for Given {
    fn bytes(&self, r: &Resource) -> Option<&[u8]> {
        self.bytes.get(&r.binding).map(Vec::as_slice)
    }
    fn texture(&self, r: &Resource) -> Option<&Texels> {
        self.textures.get(&r.binding)
    }
    fn sampler(&self, r: &Resource) -> Option<SamplerDesc> {
        self.samplers.get(&r.binding).copied()
    }
    fn builtin(&self, b: BuiltinInput) -> Option<V> {
        self.builtins.iter().find(|(x, _)| *x == b).map(|(_, v)| v.clone())
    }
    fn varyings(&self) -> Option<V> {
        self.varyings.clone()
    }
    fn derivative(&mut self, j: u32, op: Builtin, operand: &V) -> V {
        let j = j as usize;
        if self.operands.len() <= j {
            self.operands.resize(j + 1, zero_like(operand));
            self.ops.resize(j + 1, op);
        }
        self.operands[j] = operand.clone();
        self.ops[j] = op;
        self.answers.get(j).cloned().unwrap_or_else(|| zero_like(operand))
    }
}

/// A `u32` vector of three, as an id builtin is given.
pub fn uvec3(x: u32, y: u32, z: u32) -> V {
    V::P(vec![V::S(Const::U32(x)), V::S(Const::U32(y)), V::S(Const::U32(z))])
}

/// An `f32` vector.
pub fn fvec(xs: &[f32]) -> V {
    V::P(xs.iter().map(|x| V::S(Const::F32(*x))).collect())
}

/// A value's floats, in order, however it's nested.
pub fn floats_of(v: &V) -> Vec<f32> {
    match v {
        V::S(Const::F32(x)) => vec![*x],
        V::S(Const::F16(x)) => vec![*x],
        V::S(_) => Vec::new(),
        V::P(ps) => ps.iter().flat_map(floats_of).collect(),
    }
}

fn f(v: &V) -> Result<f32> {
    match v {
        V::S(Const::F32(x)) => Ok(*x),
        other => Err(Error::Malformed(format!("a texture argument of {other:?}"))),
    }
}

fn u(v: &V) -> Result<i64> {
    match v {
        V::S(Const::U32(x)) => Ok(i64::from(*x)),
        V::S(Const::I32(x)) => Ok(i64::from(*x)),
        other => Err(Error::Malformed(format!("a texel coordinate of {other:?}"))),
    }
}

fn fs(v: &V) -> Result<Vec<f32>> {
    match v {
        V::P(ps) => ps.iter().map(f).collect(),
        s => Ok(vec![f(s)?]),
    }
}

/// Texel `(x, y)` of `layer` at `level`, as four floats (alpha 1 where the format has none), or
/// `None` where it's outside the level.
fn texel(t: &Texels, level: usize, layer: usize, x: i64, y: i64, z: i64) -> Option<[f32; 4]> {
    let (w, h) = ((t.width >> level).max(1) as i64, (t.height >> level).max(1) as i64);
    let d = if t.depth > 1 && t.levels.first().is_some_and(|l| l.len() == 1) {
        (t.depth >> level).max(1) as i64
    } else {
        1
    };
    if x < 0 || y < 0 || z < 0 || x >= w || y >= h || z >= d {
        return None;
    }
    let bytes = t.levels.get(level)?.get(layer)?;
    let n = t.format.bytes_per_texel() as usize;
    let at = (((z * h + y) * w + x) as usize) * n;
    let b = bytes.get(at..at + n)?;
    let w32 = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    let h16 = |i: usize| super::half_to_f32(u16::from_le_bytes([b[2 * i], b[2 * i + 1]]));
    let unorm = |i: usize| f32::from(b[i]) / 255.0;
    use TextureFormat as T;
    Some(match t.format {
        T::Rgba8 => [unorm(0), unorm(1), unorm(2), unorm(3)],
        T::R8 => [unorm(0), 0.0, 0.0, 1.0],
        T::Rg8 => [unorm(0), unorm(1), 0.0, 1.0],
        T::Rgba16Float => [h16(0), h16(1), h16(2), h16(3)],
        T::R16Float => [h16(0), 0.0, 0.0, 1.0],
        T::Rg16Float => [h16(0), h16(1), 0.0, 1.0],
        T::R32Float | T::Depth32Float => [f32::from_bits(w32(0)), 0.0, 0.0, 1.0],
        T::Rg32Float => [f32::from_bits(w32(0)), f32::from_bits(w32(1)), 0.0, 1.0],
        T::Rgba32Float => [0, 1, 2, 3].map(|i| f32::from_bits(w32(i))),
        T::R32Uint => [f32::from_bits(w32(0)), 0.0, 0.0, 1.0],
        T::Rg32Uint => [f32::from_bits(w32(0)), f32::from_bits(w32(1)), 0.0, 1.0],
        T::Rgb10a2 => {
            let v = w32(0);
            let c = |s: u32, bits: u32| {
                ((v >> s) & ((1 << bits) - 1)) as f32 / ((1 << bits) - 1) as f32
            };
            [c(0, 10), c(10, 10), c(20, 10), c(30, 2)]
        }
        T::Rg11b10Float => {
            let v = w32(0);
            // Unsigned floats: 6 mantissa bits (5 for blue) and 5 exponent bits.
            let uf = |bits: u32, m: u32| {
                let (e, man) = ((bits >> m) & 31, bits & ((1 << m) - 1));
                let scale = (1u32 << m) as f32;
                match e {
                    0 => man as f32 / scale * 2f32.powi(-14),
                    31 => f32::INFINITY,
                    _ => (1.0 + man as f32 / scale) * 2f32.powi(e as i32 - 15),
                }
            };
            [uf(v & 0x7ff, 6), uf((v >> 11) & 0x7ff, 6), uf(v >> 22, 5), 1.0]
        }
        _ => return None,
    })
}

/// Wraps or clamps a texel coordinate to `0..n`.
fn address(i: i64, n: i64, repeat: bool) -> i64 {
    if repeat { i.rem_euclid(n) } else { i.clamp(0, n - 1) }
}

/// A sample of a 2D level (or a layer of an array, or a cube's face) at `uv`, nearest or linear.
fn sample2(t: &Texels, s: &SamplerDesc, level: usize, layer: usize, uv: [f32; 2]) -> [f32; 4] {
    let (w, h) = ((t.width >> level).max(1) as i64, (t.height >> level).max(1) as i64);
    let at = |x: i64, y: i64| {
        texel(t, level, layer, address(x, w, s.repeat[0]), address(y, h, s.repeat[1]), 0)
            .unwrap_or([0.0; 4])
    };
    let (x, y) = (uv[0] * w as f32, uv[1] * h as f32);
    if !s.linear {
        return at(x.floor() as i64, y.floor() as i64);
    }
    let (x, y) = (x - 0.5, y - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        top + (bottom - top) * fy
    })
}

/// A sample of a 3D level at `p`.
fn sample3(t: &Texels, s: &SamplerDesc, level: usize, p: [f32; 3]) -> [f32; 4] {
    let n = [t.width, t.height, t.depth].map(|x| (x >> level).max(1) as i64);
    let at = |q: [i64; 3]| {
        let q: [i64; 3] = std::array::from_fn(|i| address(q[i], n[i], s.repeat[i]));
        texel(t, level, 0, q[0], q[1], q[2]).unwrap_or([0.0; 4])
    };
    let x: [f32; 3] = std::array::from_fn(|i| p[i] * n[i] as f32);
    if !s.linear {
        return at(x.map(|v| v.floor() as i64));
    }
    let x = x.map(|v| v - 0.5);
    let lo = x.map(f32::floor);
    let fr: [f32; 3] = std::array::from_fn(|i| x[i] - lo[i]);
    let lo = lo.map(|v| v as i64);
    let mut out = [0.0f32; 4];
    for k in 0..8 {
        let q: [i64; 3] = std::array::from_fn(|i| lo[i] + ((k >> i) & 1) as i64);
        let w: f32 = (0..3).map(|i| if (k >> i) & 1 == 1 { fr[i] } else { 1.0 - fr[i] }).product();
        let v = at(q);
        for c in 0..4 {
            out[c] += w * v[c];
        }
    }
    out
}

/// A cube's face and its `uv` for a direction (WebGPU's cube face order: +x, -x, +y, -y, +z, -z).
fn cube_face(d: [f32; 3]) -> (usize, [f32; 2]) {
    let [x, y, z] = d;
    let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
    let (face, sc, tc, ma) = if ax >= ay && ax >= az {
        if x > 0.0 { (0, -z, -y, ax) } else { (1, z, -y, ax) }
    } else if ay >= az {
        if y > 0.0 { (2, x, z, ay) } else { (3, x, -z, ay) }
    } else if z > 0.0 {
        (4, x, -y, az)
    } else {
        (5, -x, -y, az)
    };
    (face, [0.5 * (sc / ma + 1.0), 0.5 * (tc / ma + 1.0)])
}

/// A texture read (see the module docs): the value, and whether it's approximate. `deriv`
/// answers an implicit-level sample's derivatives (`derivatives` counts the invocation's calls).
#[allow(clippy::too_many_arguments)]
pub fn texture(
    op: TextureOp,
    t: &Texels,
    dim: TexDim,
    depth: bool,
    sampler: Option<SamplerDesc>,
    args: &[V],
    deriv: &mut dyn FnMut(u32, Builtin, &V) -> V,
    derivatives: &mut u32,
) -> Result<(V, bool)> {
    let levels = t.levels.len().max(1);
    let one = |c: [f32; 4]| if depth { V::S(Const::F32(c[0])) } else { super::concrete::fvec(&c) };
    let uint = matches!(t.format, TextureFormat::R32Uint | TextureFormat::Rg32Uint);
    let out = |c: [f32; 4]| {
        if uint { V::P(c.iter().map(|x| V::S(Const::U32(x.to_bits()))).collect()) } else { one(c) }
    };
    match op {
        TextureOp::Width | TextureOp::Height | TextureOp::Depth | TextureOp::Layers => {
            let n = match op {
                TextureOp::Width => t.width,
                TextureOp::Height => t.height,
                _ => t.depth,
            };
            return Ok((V::S(Const::U32(n)), false));
        }
        TextureOp::Levels => return Ok((V::S(Const::U32(levels as u32)), false)),
        TextureOp::Load => {
            let n = dim.texel();
            let xs: Vec<i64> = args[..n].iter().map(u).collect::<Result<_>>()?;
            let mut rest = &args[n..];
            let layer = if dim.arrayed() {
                let l = u(&rest[0])? as usize;
                rest = &rest[1..];
                l
            } else {
                0
            };
            let level = rest.first().map(u).transpose()?.unwrap_or(0) as usize;
            let z = if n == 3 { xs[2] } else { 0 };
            let c = texel(t, level, layer, xs[0], xs[1], z).unwrap_or([0.0, 0.0, 0.0, 0.0]);
            return Ok((out(c), false));
        }
        _ => {}
    }
    let s = sampler.ok_or_else(|| Error::Malformed("a sample with no sampler".into()))?;
    let coords = fs(&args[0])?;
    let mut rest = &args[1..];
    let layer = if dim.arrayed() {
        let l = u(&rest[0])? as usize;
        rest = &rest[1..];
        l
    } else {
        0
    };
    let size = [t.width as f32, t.height as f32, t.depth as f32];
    // The level a footprint picks: log2 of its larger side in texels.
    let lod = |dx: &[f32], dy: &[f32]| {
        let len =
            |d: &[f32]| d.iter().zip(size).map(|(x, n)| (x * n) * (x * n)).sum::<f32>().sqrt();
        len(dx).max(len(dy)).max(1e-30).log2()
    };
    let mut implicit = |derivatives: &mut u32| -> f32 {
        let dx = deriv(*derivatives, Builtin::Dpdx, &args[0]);
        *derivatives += 1;
        let dy = deriv(*derivatives, Builtin::Dpdy, &args[0]);
        *derivatives += 1;
        lod(&floats_of(&dx), &floats_of(&dy))
    };
    let level = match op {
        TextureOp::SampleLevel => f(&rest[0])?,
        TextureOp::SampleBias => implicit(derivatives) + f(&rest[0])?,
        TextureOp::SampleGrad => lod(&fs(&rest[0])?, &fs(&rest[1])?),
        TextureOp::Sample | TextureOp::SampleCompare => implicit(derivatives),
        _ => 0.0,
    };
    let level = level.clamp(0.0, (levels - 1) as f32);
    let at_level = |l: usize| -> [f32; 4] {
        match dim {
            TexDim::D3 => sample3(t, &s, l, [coords[0], coords[1], coords[2]]),
            TexDim::Cube => {
                let (face, uv) = cube_face([coords[0], coords[1], coords[2]]);
                sample2(t, &SamplerDesc { repeat: [false; 3], ..s }, l, face, uv)
            }
            _ => sample2(t, &s, l, layer, [coords[0], coords[1]]),
        }
    };
    let c = if s.mip_linear {
        let (l0, fr) = (level.floor() as usize, level.fract());
        let (a, b) = (at_level(l0), at_level((l0 + 1).min(levels - 1)));
        std::array::from_fn(|i| a[i] + (b[i] - a[i]) * fr)
    } else {
        at_level(level.round() as usize)
    };
    let approximate = s.anisotropy > 1;
    match op {
        TextureOp::SampleCompare | TextureOp::SampleCompareLevel => {
            let reference = f(&rest[rest.len() - 1])?;
            let pass = |d: f32| match s.compare.unwrap_or(BinOp::Le) {
                BinOp::Lt => reference < d,
                BinOp::Le => reference <= d,
                BinOp::Gt => reference > d,
                BinOp::Ge => reference >= d,
                BinOp::Eq => reference == d,
                BinOp::Ne => reference != d,
                _ => true,
            };
            // Each texel compared, then filtered: as the nearest one's, or the share passing.
            let v = if pass(c[0]) { 1.0 } else { 0.0 };
            Ok((V::S(Const::F32(v)), approximate))
        }
        TextureOp::Gather(_) | TextureOp::GatherCompare => {
            Err(Error::Unsupported("a gather".into()))
        }
        _ => Ok((out(c), approximate)),
    }
}
