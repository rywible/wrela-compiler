// The grazer: a large quadruped herbivore (bison x deer x camel), standing.
// Metres, +y up, ground at y = 0, facing +z. The whole field is evaluated on
// mirror_x(p), so every paired part is authored once on the +x (right) side.

// ---------------- skeleton ----------------
// Midline chain
const PELVIS   = vec3f(0.0, 1.30, 0.00);
const SPINE    = vec3f(0.0, 1.36, 0.50);
const CHEST    = vec3f(0.0, 1.36, 1.00);
const NECK0    = vec3f(0.0, 1.28, 1.12);
const NECK1    = vec3f(0.0, 1.48, 1.38);
const NECK2    = vec3f(0.0, 1.68, 1.51);
const NECK3    = vec3f(0.0, 1.86, 1.60);
const HEAD     = vec3f(0.0, 2.00, 1.66);   // poll; head bone points forward-down
const HEAD_PITCH = 0.65;                   // radians below horizontal
const TAIL0    = vec3f(0.0, 1.36, -0.30);
const TAIL1    = vec3f(0.0, 1.30, -0.37);
const TAIL2    = vec3f(0.0, 1.20, -0.41);
const TAIL3    = vec3f(0.0, 1.07, -0.42);
const TAIL4    = vec3f(0.0, 0.94, -0.42);
const TAIL5    = vec3f(0.0, 0.81, -0.415);
const TAIL_END = vec3f(0.0, 0.68, -0.41);
// Right hind leg (parent PELVIS)
const HIP      = vec3f(0.21, 1.18, 0.00);
const STIFLE   = vec3f(0.22, 0.71, 0.17);   // femur 0.50, bends forward
const HOCK     = vec3f(0.20, 0.32, -0.055); // tibia 0.45, bends back
const HFETLOCK = vec3f(0.20, 0.10, -0.035); // cannon 0.22
// Right fore leg (parent CHEST)
const SCAPULA  = vec3f(0.22, 1.48, 0.86);   // top of shoulder blade (withers)
const SHOULDER = vec3f(0.26, 1.12, 1.18);   // point of shoulder
const ELBOW    = vec3f(0.23, 0.73, 0.96);   // humerus 0.45, slopes back
const KNEE     = vec3f(0.21, 0.31, 0.97);   // forearm 0.42, vertical
const FFETLOCK = vec3f(0.205, 0.10, 0.99);  // cannon 0.21
// Attachments in head space (x lateral, y up, z along the head)
const EYE      = vec3f(0.096, 0.03, 0.185);
const EAR_BASE = vec3f(0.088, 0.035, -0.015);
const HORN0    = vec3f(0.07, 0.085, 0.045);
const HORN1    = vec3f(0.165, 0.10, 0.025);
const HORN2    = vec3f(0.205, 0.16, 0.01);
const HORN3    = vec3f(0.20, 0.215, 0.03);

// ---------------- helpers ----------------
fn sstep(a: f32, b: f32, x: f32) -> f32 {
  let t = clamp((x - a) / (b - a), 0.0, 1.0);
  return t * t * (3.0 - 2.0 * t);
}

// Ellipsoid: the library bound outside, a Lipschitz-1 lower bound inside (the library
// bound is discontinuous at the centre, which shows up as huge gradients near small parts).
fn ell(p: vec3f, r: vec3f) -> f32 {
  let k0 = length(p / r);
  if (k0 < 1.0) { return (k0 - 1.0) * min(r.x, min(r.y, r.z)); }
  return sd_ellipsoid(p, r);
}

// Local frame of the bone a->b: z along the bone (0 at a), x lateral, y = z cross x.
// For a leg bone pointing down, local +y is anterior (forward).
fn bone_space(p: vec3f, a: vec3f, b: vec3f) -> vec3f {
  let d = normalize(b - a);
  let sx = normalize(vec3f(1.0, 0.0, 0.0) - d * d.x);
  let sy = cross(d, sx);
  let q = p - a;
  return vec3f(dot(q, sx), dot(q, sy), dot(q, d));
}

// Ellipsoidal muscle mass on bone a->b at fraction t, offset (x, y) in the bone frame.
fn muscle(p: vec3f, a: vec3f, b: vec3f, t: f32, off: vec2f, r: vec3f) -> f32 {
  let q = bone_space(p, a, b);
  return ell(q - vec3f(off.x, off.y, t * length(b - a)), r);
}

// Round cone on the midline, narrowed laterally by factor s (zero set exact, distance a bound).
fn cone_sq(p: vec3f, a: vec3f, b: vec3f, r1: f32, r2: f32, s: f32) -> f32 {
  let q = vec3f(p.x * s, p.y, p.z);
  return sd_round_cone(q, a, b, r1, r2) / s;
}

fn head_space(p: vec3f) -> vec3f {
  return rot_x(p - HEAD, -HEAD_PITCH);
}

// ---------------- parts ----------------
fn torso(p: vec3f) -> f32 {
  var d = ell(p - vec3f(0.0, 1.10, 0.53), vec3f(0.37, 0.33, 0.60));            // barrel
  d = smin(d, ell(p - vec3f(0.0, 1.14, 0.96), vec3f(0.36, 0.35, 0.34)), 0.14); // chest
  d = smin(d, ell(p - vec3f(0.0, 1.21, 0.02), vec3f(0.30, 0.27, 0.36)), 0.14); // rump
  d = smin(d, ell(p - vec3f(0.0, 1.49, 0.80), vec3f(0.23, 0.24, 0.40)), 0.12); // shoulder hump
  d = smin(d, ell(p - vec3f(0.24, 1.26, 0.14), vec3f(0.06, 0.05, 0.07)), 0.08); // hip point (hook)
  return d;
}

fn neck(p: vec3f) -> f32 {
  var d = cone_sq(p, NECK0, NECK1, 0.30, 0.22, 1.3);
  d = smin(d, sd_round_cone(p, NECK0 + vec3f(0.0, -0.02, -0.06), NECK0 + vec3f(0.0, 0.08, 0.06), 0.30, 0.22), 0.06);
  d = smin(d, cone_sq(p, NECK1, NECK2, 0.22, 0.175, 1.3), 0.03);
  d = smin(d, cone_sq(p, NECK2, NECK3, 0.175, 0.14, 1.3), 0.03);
  d = smin(d, cone_sq(p, NECK3, HEAD, 0.14, 0.10, 1.3), 0.03);
  // mane ridge along the top of the neck
  d = smin(d, cone_sq(p, vec3f(0.0, 1.74, 1.40), vec3f(0.0, 1.99, 1.58), 0.045, 0.035, 2.0), 0.05);
  // throat beard hanging under the jaw and throat
  d = smin(d, cone_sq(p, vec3f(0.0, 1.76, 1.68), vec3f(0.0, 1.48, 1.56), 0.03, 0.06, 1.6), 0.06);
  // dewlap keel down the front of the chest
  d = smin(d, cone_sq(p, vec3f(0.0, 1.28, 1.24), vec3f(0.0, 0.88, 1.12), 0.07, 0.08, 2.2), 0.08);
  return d;
}

fn ear_space(h: vec3f) -> vec3f {
  let dir = normalize(vec3f(0.9, 0.15, -0.4));
  let fwd = normalize(vec3f(0.5, 0.3, 0.8));
  let sx = normalize(fwd - dir * dot(fwd, dir));
  let sy = cross(dir, sx);
  let q = h - EAR_BASE;
  return vec3f(dot(q, sx), dot(q, sy), dot(q, dir));
}

fn horn(h: vec3f) -> f32 {
  var d = smin(sd_round_cone(h, HORN0, HORN1, 0.036, 0.026), sd_round_cone(h, HORN1, HORN2, 0.026, 0.018), 0.012);
  return smin(d, sd_round_cone(h, HORN2, HORN3, 0.018, 0.009), 0.008);
}

// Leaf-shaped ear, thin along local x, cupped on its front face.
fn ear(h: vec3f) -> f32 {
  let e = ear_space(h);
  var d = sd_round_cone(vec3f(e.x * 1.9, e.y, e.z), vec3f(0.0, 0.0, 0.03), vec3f(0.0, 0.0, 0.15), 0.042, 0.014) / 1.9;
  let cup = sd_round_cone(vec3f((e.x - 0.022) * 1.9, e.y, e.z), vec3f(0.0, 0.0, 0.05), vec3f(0.0, 0.0, 0.145), 0.031, 0.009) / 1.9;
  return ssub(d, cup, 0.004);
}

fn head(p: vec3f) -> f32 {
  let h = head_space(p);
  var d = ell(h - vec3f(0.0, 0.02, 0.06), vec3f(0.115, 0.105, 0.13));                 // cranium
  d = smin(d, sd_round_cone(h, vec3f(0.0, 0.0, 0.10), vec3f(0.0, -0.025, 0.38), 0.10, 0.07), 0.05); // face (nasal bridge)
  d = smin(d, ell(h - vec3f(0.0, -0.04, 0.43), vec3f(0.072, 0.075, 0.08)), 0.04);     // muzzle
  d = smin(d, ell(h - vec3f(0.0, -0.068, 0.47), vec3f(0.06, 0.028, 0.05)), 0.02);    // upper lip, slightly overhanging
  d = smin(d, sd_round_cone(h, vec3f(0.0, -0.075, 0.05), vec3f(0.0, -0.105, 0.40), 0.07, 0.034), 0.04); // lower jaw
  d = smin(d, ell(h - vec3f(0.0, -0.115, 0.42), vec3f(0.042, 0.02, 0.042)), 0.02);    // lower lip / chin
  d = smin(d, ell(h - vec3f(0.065, -0.055, 0.12), vec3f(0.05, 0.075, 0.09)), 0.03);   // cheek (masseter)
  d = smin(d, ell(h - vec3f(0.08, 0.066, 0.18), vec3f(0.032, 0.018, 0.05)), 0.02);    // brow
  d = smin(d, sd_sphere(h - EYE, 0.028), 0.008);                                       // eye
  d = ssub(d, sd_box(h - vec3f(0.0, -0.098, 0.49), vec3f(0.1, 0.0025, 0.08)), 0.003);  // mouth line
  d = ssub(d, ell(rot_x(h - vec3f(0.034, -0.02, 0.505), 0.5), vec3f(0.012, 0.026, 0.016)), 0.006); // nostril
  return d;
}

fn tail(p: vec3f) -> f32 {
  var d = sd_round_cone(p, TAIL0, TAIL1, 0.06, 0.045);
  d = smin(d, sd_round_cone(p, TAIL1, TAIL2, 0.045, 0.036), 0.015);
  d = smin(d, sd_round_cone(p, TAIL2, TAIL3, 0.036, 0.030), 0.015);
  d = smin(d, sd_round_cone(p, TAIL3, TAIL4, 0.030, 0.026), 0.015);
  d = smin(d, sd_round_cone(p, TAIL4, TAIL5, 0.026, 0.022), 0.015);
  d = smin(d, tuft(p), 0.03);
  return d;
}

fn tuft(p: vec3f) -> f32 {
  return muscle(p, TAIL5, TAIL_END, 0.55, vec2f(0.0), vec3f(0.04, 0.04, 0.085)) + 0.002 * noise3(p * 30.0);
}

// Hoof horn: rounded cone, flat sole on the ground, cleft between the claws.
fn hoof_horn(p: vec3f, fet: vec3f) -> f32 {
  let q = p - vec3f(fet.x, 0.0, fet.z);
  var h = sd_round_cone(q, vec3f(0.0, 0.032, 0.045), vec3f(0.0, 0.078, 0.018), 0.062, 0.043);
  h = smax(h, -q.y, 0.006);
  h = ssub(h, sd_box(q - vec3f(0.0, 0.03, 0.10), vec3f(0.004, 0.05, 0.05)), 0.004);
  return h;
}

fn pastern(p: vec3f, fet: vec3f) -> f32 {
  let q = p - vec3f(fet.x, 0.0, fet.z);
  return sd_round_cone(q, vec3f(0.0, fet.y, -0.005), vec3f(0.0, 0.068, 0.022), 0.04, 0.043);
}

fn fore_leg(q: vec3f) -> f32 {
  var d = muscle(q, SCAPULA, SHOULDER, 0.45, vec2f(0.0, -0.02), vec3f(0.12, 0.17, 0.28));          // shoulder blade mass
  d = smin(d, sd_round_cone(q, SHOULDER, ELBOW, 0.10, 0.085), 0.06);                                // upper arm
  d = smin(d, muscle(q, SHOULDER, ELBOW, 0.55, vec2f(0.0, -0.08), vec3f(0.10, 0.13, 0.2)), 0.06);   // triceps
  d = smin(d, sd_sphere(q - (ELBOW + vec3f(0.0, 0.02, -0.065)), 0.05), 0.04);                      // point of elbow
  d = smin(d, sd_round_cone(q, ELBOW, KNEE, 0.10, 0.048), 0.04);                                  // forearm
  d = smin(d, muscle(q, ELBOW, KNEE, 0.2, vec2f(0.0, 0.01), vec3f(0.085, 0.115, 0.17)), 0.05);     // forearm muscles
  d = smin(d, ell(q - KNEE, vec3f(0.058, 0.056, 0.055)), 0.03);                                   // knee (carpus)
  d = smin(d, sd_round_cone(q, KNEE, FFETLOCK, 0.040, 0.037), 0.02);                               // cannon
  d = smin(d, sd_sphere(q - (FFETLOCK + vec3f(0.0, 0.0, -0.01)), 0.045), 0.025);                   // fetlock
  d = smin(d, pastern(q, FFETLOCK), 0.02);
  d = smin(d, hoof_horn(q, FFETLOCK), 0.01);
  return d;
}

fn hind_leg(q: vec3f) -> f32 {
  var d = muscle(q, HIP, STIFLE, 0.5, vec2f(0.0, -0.10), vec3f(0.15, 0.27, 0.36));                 // thigh
  d = smin(d, sd_round_cone(q, vec3f(0.13, 1.18, -0.24), vec3f(0.18, 0.66, -0.07), 0.12, 0.07), 0.09);  // hamstrings
  d = smin(d, sd_sphere(q - (STIFLE + vec3f(0.015, 0.0, 0.05)), 0.065), 0.06);                    // stifle (patella)
  d = smin(d, sd_round_cone(q, STIFLE, HOCK, 0.10, 0.052), 0.06);                                  // gaskin
  d = smin(d, muscle(q, STIFLE, HOCK, 0.32, vec2f(0.0, -0.05), vec3f(0.07, 0.08, 0.16)), 0.05);     // calf
  d = smin(d, sd_capsule(q, mix(STIFLE, HOCK, 0.5) + vec3f(0.0, 0.0, -0.07), HOCK + vec3f(0.0, 0.04, -0.07), 0.022), 0.04); // achilles
  d = smin(d, sd_sphere(q - (HOCK + vec3f(0.0, 0.04, -0.07)), 0.035), 0.02);                      // point of hock
  d = smin(d, ell(q - HOCK, vec3f(0.055, 0.058, 0.06)), 0.03);                                    // hock joint
  d = smin(d, sd_round_cone(q, HOCK, HFETLOCK, 0.043, 0.038), 0.02);                               // cannon
  d = smin(d, sd_sphere(q - (HFETLOCK + vec3f(0.0, 0.0, -0.01)), 0.046), 0.025);                  // fetlock
  d = smin(d, pastern(q, HFETLOCK), 0.02);
  d = smin(d, hoof_horn(q, HFETLOCK), 0.01);
  return d;
}

// Where the woolly cape grows: hump, neck, shoulders, throat, upper fore legs; not the face.
fn cape_mask(p: vec3f) -> f32 {
  let zs = p.z + 0.6 * (p.y - 1.2) + 0.08 * noise3(p * 4.0);
  let fore = sstep(0.35, 0.65, zs) * sstep(0.8, 1.05, p.y);
  let chaps = 0.4 * sstep(0.7, 0.88, p.z) * sstep(1.23, 1.06, p.z) * sstep(0.58, 0.8, p.y);
  let face = sstep(0.2, 0.36, length(head_space(p) - vec3f(0.0, -0.02, 0.32)));
  return max(fore, chaps) * face;
}

// ---------------- the creature ----------------
fn field(p: vec3f) -> f32 {
  let q = mirror_x(p);
  var d = torso(q);
  d = smin(d, neck(q), 0.10);
  d = smin(d, head(q), 0.05);
  d = smin(d, tail(q), 0.05);
  d = smin(d, fore_leg(q), 0.07);
  d = smin(d, hind_leg(q), 0.08);
  // woolly cape: inflate and roughen; divide to keep the field 1-Lipschitz
  let m = cape_mask(q);
  if (m > 0.0) {
    let shag = m * (0.016 + 0.014 * fbm3(p * vec3f(9.0, 5.0, 9.0), 3) + 0.012 * noise3(p * 4.0 + 5.0));
    d = (d - shag) / (1.0 + 0.4 * m);
  }
  let h = head_space(q);
  d = smin(d, ear(h), 0.012);
  d = smin(d, horn(h), 0.025);
  return max(d, -p.y);
}

// ---------------- colour ----------------
// Round dark dapples: jittered cells, one spot per cell.
fn dapples(p: vec3f) -> f32 {
  let s = p * 7.0 + 0.6 * vec3f(noise3(p * 3.0), noise3(p * 3.0 + 7.0), noise3(p * 3.0 + 13.0));
  let cell = floor(s);
  var best = 9.0;
  for (var i = -1; i <= 1; i++) {
    for (var j = -1; j <= 1; j++) {
      for (var k = -1; k <= 1; k++) {
        let c = cell + vec3f(f32(i), f32(j), f32(k));
        let ci = vec3i(c);
        let o = 0.5 + 0.35 * vec3f(fv_hash(ci), fv_hash(ci + vec3i(17, 31, 7)), fv_hash(ci + vec3i(5, 11, 23)));
        let r = 0.30 + 0.12 * fv_hash(ci + vec3i(3, 3, 3));
        best = min(best, length(s - c - o) - r);
      }
    }
  }
  return sstep(0.07, -0.05, best);
}

fn albedo(p: vec3f) -> vec3f {
  let q = mirror_x(p);
  let hide = vec3f(0.12, 0.062, 0.030);
  let dapple = vec3f(0.05, 0.028, 0.015);
  let wool = vec3f(0.058, 0.035, 0.02);
  let pale = vec3f(0.20, 0.13, 0.08);
  let sock = vec3f(0.05, 0.032, 0.022);

  var c = hide * (1.0 + 0.2 * fbm3(p * 2.5, 3));
  // dapples on the torso
  let torso_mask = sstep(0.86, 1.02, p.y) * sstep(1.25, 1.05, p.z) * sstep(-0.45, -0.25, p.z);
  let rump = sstep(-0.12, -0.28, p.z) * sstep(0.9, 1.1, p.y) * sstep(0.24, 0.12, q.x + 0.1 * noise3(p * 6.0));
  c = mix(c, dapple, dapples(p) * torso_mask * 0.8 * (1.0 - rump));
  c = mix(c, vec3f(0.24, 0.17, 0.10), rump * 0.85);
  // countershaded belly and inner legs
  c = mix(c, pale, sstep(0.95, 0.78, p.y) * sstep(0.62, 0.72, p.y) * sstep(0.30, 0.12, q.x) * 0.7);
  // darker lower legs
  c = mix(c, sock, sstep(0.55, 0.35, p.y));
  // dark woolly cape over the forequarters
  c = mix(c, wool * (1.0 + 0.3 * fbm3(p * 12.0, 2)), cape_mask(q) * 0.9);
  // hooves
  if (min(hoof_horn(q, FFETLOCK), hoof_horn(q, HFETLOCK)) < 0.004) {
    return vec3f(0.022, 0.019, 0.017);
  }
  // tail tuft
  if (tuft(q) < 0.006) {
    return vec3f(0.03, 0.02, 0.013);
  }
  // head
  let h = head_space(q);
  let on_head = sstep(0.62, 0.45, length(h));
  let eye_r = length(h - EYE);
  if (eye_r < 0.029) {
    return vec3f(0.008, 0.006, 0.005);
  }
  c = mix(c, vec3f(0.03, 0.02, 0.014), sstep(0.042, 0.032, eye_r));
  c = mix(c, vec3f(0.04, 0.03, 0.024), sstep(0.32, 0.42, h.z) * on_head);
  c = mix(c, vec3f(0.018, 0.015, 0.014), sstep(0.47, 0.50, h.z) * sstep(-0.09, -0.075, h.y) * on_head);
  if (horn(h) < 0.004) {
    return mix(vec3f(0.035, 0.03, 0.026), vec3f(0.012, 0.011, 0.010), sstep(0.0, 0.14, length(h - HORN0)));
  }
  if (ear(h) < 0.004) {
    let e = ear_space(h);
    c = mix(wool, vec3f(0.17, 0.11, 0.09), sstep(0.0, 0.006, e.x));
  }
  return c;
}
