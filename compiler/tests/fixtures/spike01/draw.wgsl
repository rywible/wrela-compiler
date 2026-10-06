// Drawing (sketch 02 §7): skinning, the three shading modes, the creature shadow, terrain.
// Appended to field.wgsl.

struct View {
  viewproj: mat4x4f,
  light: mat4x4f,       // the sun's view-projection, for the shadow map
  eye: vec4f,
  sun: vec4f,           // xyz: direction toward the sun
}

@group(0) @binding(0) var<uniform> V: View;
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_samp: sampler_comparison;
@group(0) @binding(3) var detail: texture_3d<f32>;
@group(0) @binding(4) var detail_samp: sampler;
@group(1) @binding(0) var<uniform> G: Grazer;
@group(1) @binding(1) var<storage, read> palette: array<mat4x4f, 29>;

// Sketch 01's colours, converted from sRGB to linear.
const HIDE = vec3f(0.148, 0.089, 0.047);
const HIDE_DARK = vec3f(0.060, 0.037, 0.021);
const HOOF = vec3f(0.0130, 0.0090, 0.0070);

struct VIn {
  @location(0) pos: vec3f,
  @location(1) bones: vec4u,
  @location(2) nrm: vec3f,
  @location(3) weights: vec4f,
  @location(4) parts: u32,
}

struct VOut {
  @builtin(position) @invariant clip: vec4f,       // invariant: the depth prepass must match exactly
  @location(0) rest: vec3f,                        // shading happens in rest space
  @location(1) world: vec3f,
  @location(2) r0: vec3f,                          // the skinning matrix's linear part, for normals
  @location(3) r1: vec3f,
  @location(4) r2: vec3f,
  @location(5) nrm: vec3f,
  @location(6) @interpolate(flat) parts: u32,      // PartMask, not interpolated
}

fn skin_matrix(v: VIn) -> mat4x4f {
  return palette[v.bones.x] * v.weights.x + palette[v.bones.y] * v.weights.y
       + palette[v.bones.z] * v.weights.z + palette[v.bones.w] * v.weights.w;
}

@vertex
fn skin(v: VIn) -> VOut {
  let m = skin_matrix(v);
  let wp = (m * vec4f(v.pos, 1.0)).xyz;
  var o: VOut;
  o.clip = V.viewproj * vec4f(wp, 1.0);
  o.rest = v.pos;
  o.world = wp;
  o.r0 = m[0].xyz;
  o.r1 = m[1].xyz;
  o.r2 = m[2].xyz;
  o.nrm = (m * vec4f(v.nrm, 0.0)).xyz;
  o.parts = v.parts;
  return o;
}

@vertex
fn skin_shadow(v: VIn) -> @builtin(position) vec4f {
  return V.light * (skin_matrix(v) * vec4f(v.pos, 1.0));
}

// ---- lighting, shared by every mode so only the material inputs differ ------------------------

fn shadow_at(world: vec3f, n: vec3f) -> f32 {
  let lp = V.light * vec4f(world + n * 0.03, 1.0);
  let uv = lp.xy * vec2f(0.5, -0.5) + 0.5;
  return textureSampleCompareLevel(shadow_map, shadow_samp, uv, lp.z - 0.002);
}

fn tonemap(x: vec3f) -> vec3f {
  let c = x * 0.8;
  return clamp((c * (2.51 * c + 0.03)) / (c * (2.43 * c + 0.59) + 0.14), vec3f(0.0), vec3f(1.0));
}

fn light(albedo: vec3f, rough: f32, n: vec3f, world: vec3f) -> vec3f {
  let l = V.sun.xyz;
  let v = normalize(V.eye.xyz - world);
  let h = normalize(l + v);
  let ndl = max(dot(n, l), 0.0);
  let a = max(rough * rough, 0.02);
  let sp = 2.0 / (a * a) - 2.0;
  let spec = pow(max(dot(n, h), 0.0), sp) * (sp + 8.0) / 25.13 * 0.04;
  let sh = shadow_at(world, n);
  let sky = mix(vec3f(0.30, 0.26, 0.20), vec3f(0.42, 0.55, 0.75), 0.5 + 0.5 * n.y);
  let sun = vec3f(3.2, 2.9, 2.5);
  return tonemap(albedo * (sky * 0.55 + sun * ndl * sh) + sun * spec * ndl * sh);
}

// Dapples on the torso: thresholded noise whose edge widens with the footprint (D-077).
fn dapple(p: vec3f, fp: f32) -> f32 {
  let f = G.seed.w;
  let q = (p + G.seed.xyz) * f;
  let n = noise_d(q) + 0.5 * noise_d(q * 2.0 + 31.0);
  let edge = max(0.08, fp * f * 2.0);
  return smoothstep(0.25 - edge, 0.25 + edge, n);
}

// ---- shading mode 1: per-pixel field evaluation (the thesis) -----------------------------------

@fragment
fn shade_field(i: VOut) -> @location(0) vec4f {
  let fp = length(fwidth(i.rest));                 // this pixel's footprint in rest space
  let s = grazer_g(i.rest, i.parts, fp);           // 2–3 parts, not 20; sub-pixel octaves fade
  let n = normalize(mat3x3f(i.r0, i.r1, i.r2) * s.g);
  var hide = HIDE;
  if (s.ch.x > 0.0) { hide = mix(HIDE, HIDE_DARK, dapple(i.rest, fp) * s.ch.x); }
  let albedo = mix(hide, HOOF, s.ch.y);
  return vec4f(light(albedo, mix(0.7, 0.35, s.ch.y), n, i.world), 1.0);
}

// ---- shading mode 2: baked lookups (cost proxy) ------------------------------------------------
// Two trilinear, mipmapped texture fetches standing in for a baked normal map and albedo map.
// A shared noise texture, not a real bake of this creature: it measures cost, not quality.

@fragment
fn shade_lookup(i: VOut) -> @location(0) vec4f {
  let a = textureSample(detail, detail_samp, i.rest * 1.7);
  let b = textureSample(detail, detail_samp, i.rest * 0.45 + 0.3);
  let n = normalize(normalize(i.nrm) + (a.xyz * 2.0 - 1.0) * 0.25);
  let albedo = mix(HIDE, HIDE_DARK, smoothstep(0.45, 0.55, b.w));
  return vec4f(light(albedo, 0.7, n, i.world), 1.0);
}

// ---- shading mode 3: per-vertex normals, no detail (the floor) ---------------------------------

@fragment
fn shade_vertex(i: VOut) -> @location(0) vec4f {
  return vec4f(light(HIDE, 0.7, normalize(i.nrm), i.world), 1.0);
}

// ---- terrain: a plain displaced grid; not what this spike measures ----------------------------

const TERRAIN_N: u32 = 256u;
const TERRAIN_SIZE: f32 = 160.0;

fn terrain_h(x: f32, z: f32) -> f32 {
  return 0.6 * sin(0.11 * x) * cos(0.09 * z)
       + 0.3 * sin(0.23 * x + 1.3) * sin(0.19 * z + 0.4)
       + 0.08 * sin(0.9 * x) * cos(0.7 * z);
}

struct TOut {
  @builtin(position) clip: vec4f,
  @location(0) world: vec3f,
  @location(1) nrm: vec3f,
}

@vertex
fn terrain_vs(@builtin(vertex_index) vi: u32) -> TOut {
  var qx = array<u32, 6>(0u, 1u, 1u, 0u, 1u, 0u);
  var qz = array<u32, 6>(0u, 0u, 1u, 0u, 1u, 1u);
  let q = vi / 6u;
  let c = vi % 6u;
  let gx = f32(q % TERRAIN_N + qx[c]) / f32(TERRAIN_N) - 0.5;
  let gz = f32(q / TERRAIN_N + qz[c]) / f32(TERRAIN_N) - 0.5;
  let x = gx * TERRAIN_SIZE;
  let z = gz * TERRAIN_SIZE;
  let e = 0.1;
  var o: TOut;
  o.world = vec3f(x, terrain_h(x, z), z);
  o.nrm = normalize(vec3f(terrain_h(x - e, z) - terrain_h(x + e, z), 2.0 * e,
                          terrain_h(x, z - e) - terrain_h(x, z + e)));
  o.clip = V.viewproj * vec4f(o.world, 1.0);
  return o;
}

@fragment
fn terrain_fs(i: TOut) -> @location(0) vec4f {
  let g = noise_d(vec3f(i.world.x, 0.0, i.world.z) * 0.35) * 0.5 + 0.5;
  let albedo = mix(vec3f(0.035, 0.060, 0.012), vec3f(0.075, 0.085, 0.020), g);
  return vec4f(light(albedo, 0.9, normalize(i.nrm), i.world), 1.0);
}

// Measurement only: marks visible creature pixels (drawn with depth 'equal' after the prepass).
@fragment
fn coverage(i: VOut) -> @location(0) vec4f {
  return vec4f(1.0);
}

// Debug only (not measured): channels and mask as colour.
@fragment
fn shade_debug(i: VOut) -> @location(0) vec4f {
  let s = grazer_g(i.rest, i.parts, 0.0);
  return vec4f(s.ch.x, s.ch.y, f32(countOneBits(i.parts)) / 20.0, 1.0);
}
