// ---- fieldview's helper library: common SDF primitives and operators (available to field files) ----

fn sd_sphere(p: vec3f, r: f32) -> f32 {
  return length(p) - r;
}

fn sd_box(p: vec3f, half: vec3f) -> f32 {
  let q = abs(p) - half;
  return length(max(q, vec3f(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}

fn sd_round_box(p: vec3f, half: vec3f, r: f32) -> f32 {
  return sd_box(p, half - vec3f(r)) - r;
}

// Bound, not exact.
fn sd_ellipsoid(p: vec3f, r: vec3f) -> f32 {
  let k0 = length(p / r);
  let k1 = max(length(p / (r * r)), 1e-9);
  return k0 * (k0 - 1.0) / k1;
}

fn sd_capsule(p: vec3f, a: vec3f, b: vec3f, r: f32) -> f32 {
  let pa = p - a;
  let ba = b - a;
  let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
  return length(pa - ba * h) - r;
}

// Round cone from sphere (a, r1) to sphere (b, r2). Exact.
fn sd_round_cone(p: vec3f, a: vec3f, b: vec3f, r1: f32, r2: f32) -> f32 {
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

fn sd_torus(p: vec3f, major: f32, minor: f32) -> f32 {
  let q = vec2f(length(p.xz) - major, p.y);
  return length(q) - minor;
}

// Smooth union / subtraction / intersection; k is the blend width in metres.
fn smin(a: f32, b: f32, k: f32) -> f32 {
  let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix(b, a, h) - k * h * (1.0 - h);
}

fn smax(a: f32, b: f32, k: f32) -> f32 {
  return -smin(-a, -b, k);
}

fn ssub(a: f32, b: f32, k: f32) -> f32 {   // a minus b
  return smax(a, -b, k);
}

// Rotations (radians) and mirroring.
fn rot_x(p: vec3f, a: f32) -> vec3f { let c = cos(a); let s = sin(a); return vec3f(p.x, c * p.y - s * p.z, s * p.y + c * p.z); }
fn rot_y(p: vec3f, a: f32) -> vec3f { let c = cos(a); let s = sin(a); return vec3f(c * p.x + s * p.z, p.y, -s * p.x + c * p.z); }
fn rot_z(p: vec3f, a: f32) -> vec3f { let c = cos(a); let s = sin(a); return vec3f(c * p.x - s * p.y, s * p.x + c * p.y, p.z); }
fn mirror_x(p: vec3f) -> vec3f { return vec3f(abs(p.x), p.y, p.z); }

// Value noise in [-1, 1] and fbm.
fn fv_hash(c: vec3i) -> f32 {
  var h = bitcast<u32>(c.x) * 747796405u + bitcast<u32>(c.y) * 2891336453u + bitcast<u32>(c.z) * 277803737u;
  h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
  h = (h >> 22u) ^ h;
  return f32(h) * (2.0 / 4294967295.0) - 1.0;
}

fn noise3(x: vec3f) -> f32 {
  let fl = floor(x);
  let i = vec3i(fl);
  let f = x - fl;
  let u = f * f * (3.0 - 2.0 * f);
  return mix(mix(mix(fv_hash(i), fv_hash(i + vec3i(1, 0, 0)), u.x),
                 mix(fv_hash(i + vec3i(0, 1, 0)), fv_hash(i + vec3i(1, 1, 0)), u.x), u.y),
             mix(mix(fv_hash(i + vec3i(0, 0, 1)), fv_hash(i + vec3i(1, 0, 1)), u.x),
                 mix(fv_hash(i + vec3i(0, 1, 1)), fv_hash(i + vec3i(1, 1, 1)), u.x), u.y), u.z);
}

fn fbm3(x: vec3f, octaves: i32) -> f32 {
  var s = 0.0;
  var a = 0.5;
  var q = x;
  for (var o = 0; o < octaves; o++) {
    s += a * noise3(q);
    q = q * 2.03 + vec3f(1.7, 9.2, 3.1);
    a *= 0.5;
  }
  return s;
}
