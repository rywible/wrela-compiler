// The grazer's field, as the compiler would emit it for `GrazerField` (sketch 01 §§1–3).
//
// - One monomorphized function. The structure (which parts, which combinators) is the type
//   (D-070); every number comes from the `G` uniform, so one pipeline serves the whole herd.
// - Each primitive appears in the interpretations the compiler derives from one definition
//   (D-012, D-080): the primal `*_d` (distance) and the forward-mode derivative `*_g` (distance and
//   gradient), plus Lipschitz constants for intervals in extract.wgsl.
// - `mask` is the engine's `PartMask` (D-080): which parts can matter here. The fold skips the rest.
// - The module that includes this file declares `G: Grazer` with its own binding.

struct Grazer {
  misc: vec4f,                  // x: blend k, y: fbm amplitude, z: fbm frequency, w: fbm octaves
  seed: vec4f,                  // xyz: noise offset, w: dapple frequency
  parts: array<vec4f, 80>,      // 20 parts × 4 vec4, laid out per part type (grazer.js)
}

const PARTS: u32 = 20u;
const ALL_PARTS: u32 = 0xFFFFFu;
const BIG: f32 = 1e6;
const SALT: f32 = 0.0;          // replaced per run when measuring cold pipeline creation

// A sample of the field: distance, gradient, and the blended channel weights the shading reads.
// ch.x = torso-ness (dappled hide), ch.y = hoof-ness. Everything else is plain hide (sketch 01 §1).
struct Sample { d: f32, g: vec3f, ch: vec2f }

// ---- noise: value noise with a quintic fade; the derivative is the forward-mode transform -----

fn hash_u(x: u32) -> u32 {
  var h = x * 747796405u + 2891336453u;
  h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
  return (h >> 22u) ^ h;
}

fn lattice(c: vec3i) -> f32 {
  let u = bitcast<vec3u>(c);
  return f32(hash_u(u.x + hash_u(u.y + hash_u(u.z)))) * (2.0 / 4294967295.0) - 1.0;
}

fn noise_d(x: vec3f) -> f32 {
  let fl = floor(x);
  let i = vec3i(fl);
  let f = x - fl;
  let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
  let a = lattice(i);
  let b = lattice(i + vec3i(1, 0, 0));
  let c = lattice(i + vec3i(0, 1, 0));
  let d = lattice(i + vec3i(1, 1, 0));
  let e = lattice(i + vec3i(0, 0, 1));
  let f1 = lattice(i + vec3i(1, 0, 1));
  let g = lattice(i + vec3i(0, 1, 1));
  let h = lattice(i + vec3i(1, 1, 1));
  return mix(mix(mix(a, b, u.x), mix(c, d, u.x), u.y), mix(mix(e, f1, u.x), mix(g, h, u.x), u.y), u.z);
}

fn noise_g(x: vec3f) -> vec4f {
  let fl = floor(x);
  let i = vec3i(fl);
  let f = x - fl;
  let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
  let du = 30.0 * f * f * (f * (f - 2.0) + 1.0);
  let a = lattice(i);
  let b = lattice(i + vec3i(1, 0, 0));
  let c = lattice(i + vec3i(0, 1, 0));
  let d = lattice(i + vec3i(1, 1, 0));
  let e = lattice(i + vec3i(0, 0, 1));
  let f1 = lattice(i + vec3i(1, 0, 1));
  let g = lattice(i + vec3i(0, 1, 1));
  let h = lattice(i + vec3i(1, 1, 1));
  let k1 = b - a; let k2 = c - a; let k3 = e - a;
  let k4 = a - b - c + d; let k5 = a - c - e + g; let k6 = a - b - e + f1;
  let k7 = -a + b + c - d + e - f1 - g + h;
  let v = a + k1 * u.x + k2 * u.y + k3 * u.z + k4 * u.x * u.y + k5 * u.y * u.z + k6 * u.z * u.x
        + k7 * u.x * u.y * u.z;
  let grad = du * vec3f(k1 + k4 * u.y + k6 * u.z + k7 * u.y * u.z,
                        k2 + k5 * u.z + k4 * u.x + k7 * u.z * u.x,
                        k3 + k6 * u.x + k5 * u.y + k7 * u.x * u.y);
  return vec4f(v, grad);
}

// fbm with footprint filtering (D-077): an octave whose period is under ~2–4 footprints fades
// out instead of aliasing, and finer octaves are skipped entirely. `fp` = 0 means unfiltered.
fn octave_weight(fp: f32, freq: f32) -> f32 {
  return 1.0 - smoothstep(0.25, 0.5, fp * freq);
}

fn fbm_d(p: vec3f, fp: f32) -> f32 {
  var s = 0.0;
  var a = 1.0;
  var fr = G.misc.z;
  let octaves = u32(G.misc.w);                 // a uniform loop bound (sketch 02 §5)
  for (var o = 0u; o < octaves; o++) {
    let w = a * octave_weight(fp, fr);
    if (w <= 0.0) { break; }
    s += w * noise_d(p * fr + f32(o) * 17.0);
    a *= 0.5;
    fr *= 2.0;
  }
  return s;
}

fn fbm_g(p: vec3f, fp: f32) -> vec4f {
  var s = vec4f(0.0);
  var a = 1.0;
  var fr = G.misc.z;
  let octaves = u32(G.misc.w);
  for (var o = 0u; o < octaves; o++) {
    let w = a * octave_weight(fp, fr);
    if (w <= 0.0) { break; }
    let n = noise_g(p * fr + f32(o) * 17.0);
    s += w * vec4f(n.x, n.yzw * fr);
    a *= 0.5;
    fr *= 2.0;
  }
  return s;
}

// ---- primitives ---------------------------------------------------------------------------------

// Ellipsoid: no exact SDF exists, so this is the usual bound (sketch 01 §2: a `Bound`).
fn ellipsoid_d(p: vec3f, r: vec3f) -> f32 {
  let k0 = length(p / r);
  let k1 = max(length(p / (r * r)), 1e-9);
  return k0 * (k0 - 1.0) / k1;
}

fn ellipsoid_g(p: vec3f, r: vec3f) -> vec4f {
  let q2 = p / (r * r);
  let k0 = max(length(p / r), 1e-9);
  let k1 = max(length(q2), 1e-9);
  let dk0 = q2 / k0;
  let dk1 = (q2 / (r * r)) / k1;
  let d = k0 * (k0 - 1.0) / k1;
  return vec4f(d, ((2.0 * k0 - 1.0) * dk0 * k1 - k0 * (k0 - 1.0) * dk1) / (k1 * k1));
}

// Round cone between spheres (a, r1) and (b, r2): exact.
fn round_cone_d(p: vec3f, a: vec3f, b: vec3f, r1: f32, r2: f32) -> f32 {
  let ba = b - a;
  let l2 = dot(ba, ba);
  let rr = r1 - r2;
  let a2 = l2 - rr * rr;
  let il2 = 1.0 / l2;
  let pa = p - a;
  let y = dot(pa, ba);
  let z = y - l2;
  let xv = pa * l2 - ba * y;
  let x2 = dot(xv, xv);
  let y2 = y * y * l2;
  let z2 = z * z * l2;
  let k = sign(rr) * rr * rr * x2;
  if (sign(z) * a2 * z2 > k) { return sqrt(x2 + z2) * il2 - r2; }
  if (sign(y) * a2 * y2 < k) { return sqrt(x2 + y2) * il2 - r1; }
  return (sqrt(x2 * a2 * il2) + y * rr) * il2 - r1;
}

fn round_cone_g(p: vec3f, a: vec3f, b: vec3f, r1: f32, r2: f32) -> vec4f {
  let ba = b - a;
  let l2 = dot(ba, ba);
  let rr = r1 - r2;
  let a2 = l2 - rr * rr;
  let il2 = 1.0 / l2;
  let pa = p - a;
  let y = dot(pa, ba);
  let z = y - l2;
  let xv = pa * l2 - ba * y;
  let x2 = dot(xv, xv);
  let y2 = y * y * l2;
  let z2 = z * z * l2;
  let k = sign(rr) * rr * rr * x2;
  if (sign(z) * a2 * z2 > k) {
    let v = p - b;
    let l = max(length(v), 1e-9);
    return vec4f(l - r2, v / l);
  }
  if (sign(y) * a2 * y2 < k) {
    let l = max(length(pa), 1e-9);
    return vec4f(l - r1, pa / l);
  }
  let len = sqrt(l2);
  let u = ba / len;
  let s = rr / len;
  let perp = pa - u * dot(pa, u);
  let g = sqrt(max(1.0 - s * s, 0.0)) * perp / max(length(perp), 1e-9) + s * u;
  return vec4f((sqrt(x2 * a2 * il2) + y * rr) * il2 - r1, g);
}

// Smooth union. Where b ≥ a + k it returns exactly a, which is what makes masking exact.
// Its derivative is mix(gb, ga, h): the k·h·(1−h) term's derivative cancels.
fn smin_d(a: f32, b: f32, k: f32) -> f32 {
  let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix(b, a, h) - k * h * (1.0 - h);
}

fn smin_g(a: vec4f, b: vec4f, k: f32) -> vec4f {
  let h = clamp(0.5 + 0.5 * (b.x - a.x) / k, 0.0, 1.0);
  return vec4f(mix(b.x, a.x, h) - k * h * (1.0 - h), mix(b.yzw, a.yzw, h));
}

fn smin_s(a: Sample, b: Sample, k: f32) -> Sample {
  let h = clamp(0.5 + 0.5 * (b.d - a.d) / k, 0.0, 1.0);
  return Sample(mix(b.d, a.d, h) - k * h * (1.0 - h), mix(b.g, a.g, h), mix(b.ch, a.ch, h));
}

// ---- parts (sketch 01 §2) -----------------------------------------------------------------------

fn pv(i: u32, k: u32) -> vec4f { return G.parts[i * 4u + k]; }

// Torso without its displacement: what cull_blocks bounds (the displacement is bounded separately).
fn torso_base_d(p: vec3f) -> f32 {
  return smin_d(ellipsoid_d(p - pv(0u, 0u).xyz, pv(0u, 1u).xyz),
                ellipsoid_d(p - pv(0u, 2u).xyz, pv(0u, 3u).xyz), pv(0u, 0u).w);
}

fn torso_d(p: vec3f, fp: f32) -> f32 {
  return torso_base_d(p) + G.misc.y * fbm_d(p + G.seed.xyz, fp);
}

fn torso_g(p: vec3f, fp: f32) -> vec4f {
  let base = smin_g(ellipsoid_g(p - pv(0u, 0u).xyz, pv(0u, 1u).xyz),
                    ellipsoid_g(p - pv(0u, 2u).xyz, pv(0u, 3u).xyz), pv(0u, 0u).w);
  return base + G.misc.y * fbm_g(p + G.seed.xyz, fp);
}

fn cone_d(p: vec3f, i: u32) -> f32 {
  let a = pv(i, 0u);
  let b = pv(i, 1u);
  return round_cone_d(p, a.xyz, b.xyz, a.w, b.w);
}

fn cone_g(p: vec3f, i: u32) -> vec4f {
  let a = pv(i, 0u);
  let b = pv(i, 1u);
  return round_cone_g(p, a.xyz, b.xyz, a.w, b.w);
}

fn head_d(p: vec3f) -> f32 {
  let e = pv(2u, 0u);
  let ma = pv(2u, 2u);
  let mb = pv(2u, 3u);
  return smin_d(ellipsoid_d(p - e.xyz, pv(2u, 1u).xyz), round_cone_d(p, ma.xyz, mb.xyz, ma.w, mb.w), e.w);
}

fn head_g(p: vec3f) -> vec4f {
  let e = pv(2u, 0u);
  let ma = pv(2u, 2u);
  let mb = pv(2u, 3u);
  return smin_g(ellipsoid_g(p - e.xyz, pv(2u, 1u).xyz), round_cone_g(p, ma.xyz, mb.xyz, ma.w, mb.w), e.w);
}

// Hoof: round cone ∩ half-space below the sole. Intersection = max.
fn hoof_d(p: vec3f, i: u32) -> f32 {
  return max(cone_d(p, i), pv(i, 2u).x - p.y);
}

fn hoof_g(p: vec3f, i: u32) -> vec4f {
  let c = cone_g(p, i);
  let plane = pv(i, 2u).x - p.y;
  return select(c, vec4f(plane, 0.0, -1.0, 0.0), plane > c.x);
}

// ---- the creature: `.blend(k: 6cm)` over all parts, in a fixed fold order ----------------------

fn part_d(p: vec3f, i: u32, fp: f32) -> f32 {
  if (i == 0u) { return torso_d(p, fp); }
  if (i == 2u) { return head_d(p); }
  if (i >= 16u) { return hoof_d(p, i); }
  return cone_d(p, i);
}

fn grazer_d(p: vec3f, mask: u32, fp: f32) -> f32 {
  let k = G.misc.x;
  var d = BIG + select(0.0, 1.0, G.misc.x == SALT);
  if ((mask & 1u) != 0u) { d = smin_d(d, torso_d(p, fp), k); }
  if ((mask & 2u) != 0u) { d = smin_d(d, cone_d(p, 1u), k); }
  if ((mask & 4u) != 0u) { d = smin_d(d, head_d(p), k); }
  if ((mask & 8u) != 0u) { d = smin_d(d, cone_d(p, 3u), k); }
  for (var i = 4u; i < 16u; i++) {                    // `each_leg(... leg_segment ...)`: same type, a loop
    if (((mask >> i) & 1u) != 0u) { d = smin_d(d, cone_d(p, i), k); }
  }
  for (var i = 16u; i < 20u; i++) {
    if (((mask >> i) & 1u) != 0u) { d = smin_d(d, hoof_d(p, i), k); }
  }
  return d;
}

fn grazer_g(p: vec3f, mask: u32, fp: f32) -> Sample {
  let k = G.misc.x;
  var s = Sample(BIG + select(0.0, 1.0, G.misc.x == SALT), vec3f(0.0, 1.0, 0.0), vec2f(0.0));
  if ((mask & 1u) != 0u) { let t = torso_g(p, fp); s = smin_s(s, Sample(t.x, t.yzw, vec2f(1.0, 0.0)), k); }
  if ((mask & 2u) != 0u) { let t = cone_g(p, 1u); s = smin_s(s, Sample(t.x, t.yzw, vec2f(0.0)), k); }
  if ((mask & 4u) != 0u) { let t = head_g(p); s = smin_s(s, Sample(t.x, t.yzw, vec2f(0.0)), k); }
  if ((mask & 8u) != 0u) { let t = cone_g(p, 3u); s = smin_s(s, Sample(t.x, t.yzw, vec2f(0.0)), k); }
  for (var i = 4u; i < 16u; i++) {
    if (((mask >> i) & 1u) != 0u) { let t = cone_g(p, i); s = smin_s(s, Sample(t.x, t.yzw, vec2f(0.0)), k); }
  }
  for (var i = 16u; i < 20u; i++) {
    if (((mask >> i) & 1u) != 0u) { let t = hoof_g(p, i); s = smin_s(s, Sample(t.x, t.yzw, vec2f(0.0, 1.0)), k); }
  }
  return s;
}
