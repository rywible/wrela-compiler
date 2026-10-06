// Grey wolf (Canis lupus), adult, standing square on all four paws.
// Units: metres. +y up, ground at y = 0, +z forward (nose), +x = the wolf's left.
//
// Skeleton: every part is built from named joints below, in its bone's frame or
// between its two joints. Limb joints are given for the LEFT side (x > 0); the right
// side is the mirror image (mirror_x), so a skinned rig would duplicate them.

// ---------------- axial skeleton ----------------
const J_PELVIS  = vec3f(0.0, 0.632, -0.430);  // root: sacrum / hip centre
const J_LUMBAR  = vec3f(0.0, 0.655, -0.180);  // loin
const J_CHEST   = vec3f(0.0, 0.615,  0.095);  // thorax centre
const J_WITHERS = vec3f(0.0, 0.800,  0.200);  // top of the scapulae (shoulder height)
const J_NECK    = vec3f(0.0, 0.680,  0.310);  // neck base (C7)
const J_HEAD    = vec3f(0.0, 0.912,  0.592);  // skull centre
const HEAD_PITCH = 0.22;                      // nose-down tilt of the head bone (rad)
const HEAD_SCALE = 1.13;
// tail chain (root -> tip), hanging down and back
const J_TAIL0 = vec3f(0.0, 0.668, -0.570);
const J_TAIL1 = vec3f(0.0, 0.585, -0.660);
const J_TAIL2 = vec3f(0.0, 0.450, -0.715);
const J_TAIL3 = vec3f(0.0, 0.270, -0.690);
// ---------------- fore limb (left) ----------------
const J_SCAPULA  = vec3f(0.050, 0.750, 0.185);
const J_SHOULDER = vec3f(0.065, 0.565, 0.335);
const J_ELBOW    = vec3f(0.078, 0.415, 0.245);
const J_CARPUS   = vec3f(0.072, 0.128, 0.270);
const J_FPAW     = vec3f(0.070, 0.000, 0.300);
// ---------------- hind limb (left) ----------------
const J_HIP    = vec3f(0.070, 0.600, -0.420);
const J_STIFLE = vec3f(0.085, 0.410, -0.320);
const J_HOCK   = vec3f(0.078, 0.210, -0.495);
const J_HPAW   = vec3f(0.075, 0.000, -0.445);

// ---------------- helpers ----------------
// Ellipsoid: library bound outside; inside, (k0 - 1) * r_min, which is a true lower bound
// and avoids the bound's discontinuity at the centre.
fn sd_ell(p: vec3f, r: vec3f) -> f32 {
  let k0 = length(p / r);
  if (k0 < 1.0) { return (k0 - 1.0) * min(r.x, min(r.y, r.z)); }
  let k1 = length(p / (r * r));
  return k0 * (k0 - 1.0) / k1;
}

// Round cone squashed sideways (x scaled by 1/sx), still a valid bound.
fn sd_flat_cone(p: vec3f, a: vec3f, b: vec3f, r1: f32, r2: f32, sx: f32) -> f32 {
  let s = vec3f(sx, 1.0, 1.0);
  return sd_round_cone(p * s, a * s, b * s, r1, r2) / sx;
}

// Isosceles triangle, apex at origin, base at y = q.y (iq).
fn sd_tri_iso(p: vec2f, q: vec2f) -> f32 {
  let px = vec2f(abs(p.x), p.y);
  let a = px - q * clamp(dot(px, q) / dot(q, q), 0.0, 1.0);
  let b = px - q * vec2f(clamp(px.x / q.x, 0.0, 1.0), 1.0);
  let s = -sign(q.y);
  let dd = min(vec2f(dot(a, a), s * (px.x * q.y - px.y * q.x)),
               vec2f(dot(b, b), s * (px.y - q.y)));
  return -sqrt(dd.x) * sign(dd.y);
}

// Paw in its own frame: origin on the ground under the paw centre, +z forward.
const TOE_IN  = vec3f(0.011, 0.015, 0.030);   // paw frame (unscaled), +x half; mirrored
const TOE_OUT = vec3f(0.029, 0.014, 0.017);
fn sd_paw(q: vec3f, s: f32) -> f32 {
  var d = sd_ell(q - vec3f(0.0, 0.022, -0.012) * s, vec3f(0.030, 0.024, 0.034) * s);
  let mq = vec3f(abs(q.x), q.y, q.z);
  d = smin(d, sd_ell(mq - TOE_IN * s, vec3f(0.0125, 0.015, 0.017) * s), 0.006 * s);
  d = smin(d, sd_ell(mq - TOE_OUT * s, vec3f(0.0115, 0.014, 0.016) * s), 0.006 * s);
  return smax(d, -q.y, 0.004);
}
// Claw mask for the albedo, paw frame already divided by the scale.
fn claw_mask(pq: vec3f) -> f32 {
  let mq = vec3f(abs(pq.x), pq.y, pq.z);
  let a = mq - TOE_IN;
  let b = mq - TOE_OUT;
  let ca = smoothstep(0.011, 0.015, a.z) * (1.0 - smoothstep(0.004, 0.007, abs(a.x)));
  let cb = smoothstep(0.010, 0.014, b.z) * (1.0 - smoothstep(0.004, 0.007, abs(b.x)));
  return max(ca, cb) * (1.0 - smoothstep(0.010, 0.016, pq.y));
}

fn head_local(p: vec3f) -> vec3f { return rot_x(p - J_HEAD, -HEAD_PITCH) / HEAD_SCALE; }

const EAR_BASE = vec3f(0.046, 0.040, -0.036);   // ear joint, head frame (left; mirrored)
// Ear frame: x across the ear, y from base to tip, z through it (+z = inner face).
fn ear_local(m: vec3f) -> vec3f {
  var e = m - EAR_BASE;
  e = rot_y(e, -0.35);  // opening faces forward and a little outward
  e = rot_z(e, 0.28);   // splay outward
  e = rot_x(e, 0.04);   // stand up (head pitch already tips them forward a little)
  return e;
}

// ---------------- parts ----------------
fn sd_torso(p: vec3f) -> f32 {
  var d = sd_ell(p - J_CHEST, vec3f(0.130, 0.180, 0.305));                                // ribcage
  d = smin(d, sd_ell(p - vec3f(0.0, 0.565, 0.320), vec3f(0.085, 0.105, 0.090)), 0.07);   // prosternum
  d = smin(d, sd_ell(p - (J_WITHERS + vec3f(0.0, -0.070, 0.010)), vec3f(0.075, 0.070, 0.140)), 0.06);
  d = smin(d, sd_ell(p - J_LUMBAR, vec3f(0.066, 0.088, 0.240)), 0.08);                   // loin
  d = smin(d, sd_ell(rot_x(p - (J_PELVIS + vec3f(0.0, 0.0, -0.010)), 0.30), vec3f(0.098, 0.112, 0.145)), 0.08); // croup, sloping to the tail
  return d;
}

fn sd_neck(p: vec3f) -> f32 {
  let b = J_HEAD + vec3f(0.0, -0.030, -0.070);
  var d = sd_round_cone(p, J_NECK, b, 0.122, 0.075);
  // ruff: fur mass on the throat and sides of the neck
  let r = sd_ell(p - (J_NECK + vec3f(0.0, 0.075, 0.140)), vec3f(0.112, 0.130, 0.105)) - 0.007 * fbm3(p * 16.0, 2);
  d = smin(d, r, 0.05);
  // mane over the withers
  let mn = sd_ell(p - (J_NECK + vec3f(0.0, 0.090, -0.040)), vec3f(0.105, 0.075, 0.120)) - 0.006 * fbm3(p * 16.0 + 3.0, 2);
  d = smin(d, mn, 0.06);
  // hackles: raised fur along the top of the neck behind the head
  let hk = sd_ell(p - (J_NECK + vec3f(0.0, 0.165, 0.090)), vec3f(0.075, 0.050, 0.110)) - 0.006 * fbm3(p * 20.0 + 7.0, 2);
  d = smin(d, hk, 0.05);
  return d;
}

const EAR_H = 0.086;
// Inner hollow of the ear (the opening), in ear frame.
fn sd_ear_cavity(e: vec3f) -> f32 {
  let ci = (e - vec3f(0.0, 0.012, 0.020)) * vec3f(1.12, 1.0, 1.45);
  return sd_round_cone(ci, vec3f(0.0), vec3f(0.0, EAR_H - 0.024, 0.0), 0.030, 0.004) / 1.45;
}
// Cupped ear: a cone flattened front-to-back, hollowed from the front.
fn sd_ear(m: vec3f) -> f32 {
  let e = ear_local(m);
  let outer = sd_round_cone(e * vec3f(1.0, 1.0, 1.45), vec3f(0.0), vec3f(0.0, EAR_H, 0.0), 0.038, 0.012) / 1.45;
  return smax(outer, -sd_ear_cavity(e), 0.004);
}

// Muzzle: tapered rounded box (flat top, flat sides), in head frame.
fn sd_muzzle(q: vec3f) -> f32 {
  let z0 = 0.020;
  let z1 = 0.192;
  let t = clamp((q.z - z0) / (z1 - z0), 0.0, 1.0);
  let cy = mix(0.004, -0.005, t);
  let hx = mix(0.043, 0.022, t);
  let hy = mix(0.040, 0.026, t);
  let r = mix(0.028, 0.019, t);
  let c = vec3f(q.x, q.y - cy, q.z - 0.5 * (z0 + z1));
  return sd_box(c, vec3f(hx - r, hy - r, 0.5 * (z1 - z0) - r)) - r;
}
const EYE_C = vec3f(0.040, 0.018, 0.052);   // head frame, left eye
const EYE_GAZE = vec3f(0.74, 0.15, 0.656);  // unit, forward-outward

fn sd_head(p: vec3f) -> f32 {
  let q = head_local(p);
  let m = mirror_x(q);
  var d = sd_ell(q - vec3f(0.0, 0.010, -0.025), vec3f(0.064, 0.058, 0.072));             // cranium
  d = smin(d, sd_ell(m - vec3f(0.040, -0.018, 0.0), vec3f(0.046, 0.046, 0.056)), 0.03);  // cheeks (zygomatic + masseter)
  d = smin(d, sd_ell(m - vec3f(0.056, -0.040, -0.050), vec3f(0.045, 0.066, 0.055)) - 0.004 * fbm3(q * 40.0, 2), 0.03); // cheek ruff
  d = smin(d, sd_muzzle(q), 0.035);                                                      // muzzle
  d = smin(d, sd_flat_cone(q, vec3f(0.0, -0.044, -0.010), vec3f(0.0, -0.036, 0.152), 0.032, 0.015, 1.25), 0.012); // lower jaw
  d = smin(d, sd_ell(q - vec3f(0.0, 0.006, 0.188), vec3f(0.0155, 0.011, 0.011)), 0.006);  // nose pad
  d = smin(d, sd_ell(m - vec3f(0.030, 0.034, 0.038), vec3f(0.022, 0.014, 0.022)), 0.02); // brow
  d = min(d, sd_sphere(m - EYE_C, 0.0100));                                              // eyes
  d = smin(d, sd_ear(m), 0.014);
  return d * HEAD_SCALE;
}

fn sd_foreleg(m: vec3f) -> f32 {
  var d = sd_round_cone(m, J_SCAPULA, J_SHOULDER, 0.045, 0.058);                         // scapula
  d = smin(d, sd_round_cone(m, J_SHOULDER, J_ELBOW + vec3f(-0.008, 0.010, 0.0), 0.058, 0.040), 0.05); // humerus, elbow tucked in
  d = smin(d, sd_round_cone(m, vec3f(0.055, 0.555, 0.245), J_ELBOW + vec3f(-0.008, 0.008, -0.020), 0.055, 0.035), 0.05); // triceps
  d = smin(d, sd_flat_cone(m, J_ELBOW + vec3f(0.0, -0.010, 0.005), J_CARPUS, 0.050, 0.025, 1.2), 0.03); // forearm (muscle at the top, slim wrist)
  d = smin(d, sd_round_cone(m, J_CARPUS, J_FPAW + vec3f(0.0, 0.040, -0.008), 0.026, 0.024), 0.015); // pastern
  d = smin(d, sd_paw(m - J_FPAW, 1.25), 0.02);
  return d;
}

fn sd_hindleg(m: vec3f) -> f32 {
  // upper thigh: broad, flattened sideways; front edge runs to the stifle
  var d = sd_flat_cone(m, J_HIP + vec3f(-0.005, -0.005, -0.030), J_STIFLE + vec3f(-0.010, 0.010, -0.045), 0.112, 0.062, 1.7);
  // hamstrings: point of buttock down to the gaskin
  d = smin(d, sd_flat_cone(m, vec3f(0.060, 0.610, -0.540), vec3f(0.072, 0.330, -0.505), 0.065, 0.045, 1.4), 0.06);
  // lower thigh (gaskin): deep front-to-back at the top, tapering to the hock
  d = smin(d, sd_flat_cone(m, J_STIFLE + vec3f(-0.007, -0.010, -0.070), J_HOCK + vec3f(0.0, 0.020, -0.005), 0.058, 0.027, 1.6), 0.05);
  d = smin(d, sd_round_cone(m, vec3f(0.078, 0.360, -0.515), J_HOCK + vec3f(0.0, 0.005, -0.024), 0.030, 0.018), 0.04); // achilles
  // flank fold: skin web from belly to stifle
  d = smin(d, sd_flat_cone(m, vec3f(0.045, 0.580, -0.220), J_STIFLE + vec3f(-0.012, 0.020, -0.010), 0.045, 0.040, 1.5), 0.05);
  d = smin(d, sd_round_cone(m, J_HOCK, J_HPAW + vec3f(0.0, 0.040, -0.010), 0.024, 0.021), 0.02); // metatarsus
  d = smin(d, sd_paw(m - J_HPAW, 1.12), 0.02);
  return smax(d, 0.022 - m.x, 0.02);     // flat inner thigh; the legs never meet at the midline
}

fn sd_tail(p: vec3f) -> f32 {
  var d = sd_round_cone(p, J_TAIL0, J_TAIL1, 0.040, 0.064);
  d = smin(d, sd_round_cone(p, J_TAIL1, J_TAIL2, 0.064, 0.054), 0.02);
  d = smin(d, sd_round_cone(p, J_TAIL2, J_TAIL3, 0.054, 0.014), 0.03);
  return 0.88 * (d - 0.009 * fbm3(p * vec3f(16.0, 7.0, 16.0), 3));
}

fn field(p: vec3f) -> f32 {
  let m = mirror_x(p);
  let dh = sd_head(p);
  var d = sd_torso(p);
  d = smin(d, sd_neck(p), 0.08);
  d = smin(d, dh, 0.05);
  d = smin(d, sd_foreleg(m), 0.05);
  d = smin(d, sd_hindleg(m), 0.07);
  d = smin(d, sd_tail(p), 0.04);
  // fur: clumps stretched along the body axis; less on the lower legs
  let fur = fbm3(p * vec3f(34.0, 30.0, 13.0), 3);
  let amp = 0.0045 * smoothstep(0.12, 0.35, p.y) * mix(0.2, 1.0, smoothstep(-0.01, 0.03, dh));  // thin ears/face: little fur noise
  let tufts = 0.5 + 0.5 * noise3(p * vec3f(40.0, 9.0, 22.0));
  let belly = (1.0 - smoothstep(0.47, 0.56, p.y)) * smoothstep(-0.22, -0.10, p.z) * (1.0 - smoothstep(0.18, 0.30, p.z)) * smoothstep(0.30, 0.40, p.y);
  let britches = smoothstep(-0.48, -0.56, p.z) * smoothstep(0.30, 0.40, p.y) * (1.0 - smoothstep(0.58, 0.64, p.y)) * smoothstep(0.02, 0.05, abs(p.x));
  return 0.9 * (d - amp * fur - 0.010 * tufts * max(belly, britches));
}

// ---------------- colour ----------------
fn albedo(p: vec3f) -> vec3f {
  let m = mirror_x(p);
  let n1 = fbm3(p * 14.0, 3);
  let n2 = noise3(p * vec3f(120.0, 30.0, 120.0));
  let cream = vec3f(0.66, 0.57, 0.43);
  let tawny = vec3f(0.45, 0.30, 0.15);
  let grey  = vec3f(0.26, 0.23, 0.19);
  let dark  = vec3f(0.032, 0.028, 0.024);
  let black = vec3f(0.014, 0.012, 0.010);

  // --- body: warm grey, tawny lower flanks, dark saddle and cape, cream underside ---
  var c = mix(grey, tawny, 0.45 * (1.0 - smoothstep(0.55, 0.68, p.y + 0.04 * n1)));
  let saddle = smoothstep(0.61, 0.71, p.y + 0.07 * n1) * smoothstep(-0.64, -0.52, p.z) * (1.0 - smoothstep(0.34, 0.50, p.z));
  c = mix(c, dark, 0.85 * saddle);
  var vline = mix(0.50, 0.60, smoothstep(0.12, -0.30, p.z));
  vline = mix(vline, 0.42, smoothstep(0.05, 0.09, m.x) * smoothstep(-0.26, -0.36, p.z));
  let ventral = 1.0 - smoothstep(vline - 0.03, vline + 0.04, p.y + 0.03 * n1);
  c = mix(c, cream, ventral);
  // throat / chest bib
  let bib = (1.0 - smoothstep(0.72, 0.84, p.y + 0.03 * n1)) * smoothstep(0.27, 0.37, p.z + 0.02 * n1) * (1.0 - 0.5 * smoothstep(0.07, 0.11, m.x));
  c = mix(c, cream, bib);
  // legs: tawny outside, cream inside and below
  let legw = 1.0 - smoothstep(0.38, 0.47, p.y + 0.03 * n1);
  let legc = mix(tawny, cream, clamp(smoothstep(0.25, 0.05, p.y) * 0.6 + 0.5 * smoothstep(0.08, 0.05, m.x), 0.0, 1.0));
  c = mix(c, legc, legw);
  let fa = J_CARPUS - J_ELBOW;
  let ft = clamp(dot(m - J_ELBOW, fa) / dot(fa, fa), 0.0, 1.0);
  let fo = m - (J_ELBOW + fa * ft);
  let stripe = smoothstep(0.012, 0.022, fo.z) * (1.0 - smoothstep(0.008, 0.016, abs(fo.x))) * smoothstep(0.05, 0.2, ft) * (1.0 - smoothstep(0.75, 0.95, ft));
  c = mix(c, dark, 0.75 * stripe);
  let claws = max(claw_mask((m - J_FPAW) / 1.25), claw_mask((m - J_HPAW) / 1.12));
  c = mix(c, vec3f(0.06, 0.05, 0.045), claws);

  // --- tail ---
  let dt = sd_tail(p);
  let dbody = min(sd_torso(p), min(sd_hindleg(m), sd_neck(p)));
  let wt = clamp(0.5 + (dbody - dt) / 0.03, 0.0, 1.0);
  var tc = mix(grey, dark, 0.7 * smoothstep(-0.72, -0.62, p.z + 0.03 * n1));   // darker on top/back
  tc = mix(tc, cream, 0.5 * smoothstep(-0.68, -0.76, p.z));                     // paler underside
  tc = mix(tc, black, smoothstep(0.40, 0.36, p.y + 0.02 * n1));                 // black tip
  c = mix(c, tc, wt);

  // --- head ---
  let q = head_local(p);
  let hm = mirror_x(q);
  let wh = smoothstep(-0.14, -0.06, q.z) * smoothstep(0.70, 0.78, p.y);
  var hc = mix(grey, dark, 0.35 * smoothstep(0.0, 0.06, q.y) * (1.0 - smoothstep(0.03, 0.06, hm.x)));   // darker crown
  hc = mix(hc, mix(tawny, dark, 0.25), 0.7 * smoothstep(0.02, 0.10, q.z));      // muzzle top tawny-brown
  hc = mix(hc, cream, (1.0 - smoothstep(-0.032, -0.010, q.y + 0.01 * n1)) * (1.0 - 0.6 * smoothstep(0.012, 0.0, hm.x - 0.0) * smoothstep(-0.03, -0.02, q.y)));  // cheeks, upper lip, chin
  hc = mix(hc, cream, 0.85 * (1.0 - smoothstep(0.008, 0.016, length(hm - vec3f(0.030, 0.034, 0.050))))); // pale brow spot
  let lt = clamp((q.z - 0.020) / 0.172, 0.0, 1.0);
  let lipy = mix(0.004, -0.006, lt) - mix(0.040, 0.024, lt) + 0.003 + 0.008 * smoothstep(0.09, 0.05, q.z);
  let lip = (1.0 - smoothstep(0.0015, 0.0035, abs(q.y - lipy))) * smoothstep(0.045, 0.06, q.z);
  hc = mix(hc, dark, lip);                                                       // dark lip line, curling up at the corner
  hc = mix(hc, black, 1.0 - smoothstep(0.0, 0.004, sd_ell(q - vec3f(0.0, 0.006, 0.188), vec3f(0.0155, 0.011, 0.011)))); // nose
  let ec0 = hm - EYE_C;
  let liner = length(vec2f(ec0.z / 0.0155, (ec0.y + 0.30 * ec0.z) / 0.0085));    // almond, outer corner higher
  hc = mix(hc, dark, (1.0 - smoothstep(0.9, 1.1, liner)) * smoothstep(0.015, 0.025, hm.x));
  let gz = dot(normalize(ec0 + vec3f(1e-6)), EYE_GAZE);
  var eyec = mix(black, vec3f(0.60, 0.33, 0.05), smoothstep(0.50, 0.62, gz));   // amber iris
  eyec = mix(eyec, black, smoothstep(0.90, 0.94, gz));                           // pupil
  hc = mix(hc, eyec, 1.0 - smoothstep(0.0100, 0.0115, length(ec0)));
  // ears: tawny-grey backs with dark rims, cream inner fur
  let e = ear_local(hm);
  let inear = (1.0 - smoothstep(0.0, 0.006, sd_ear_cavity(e))) * smoothstep(0.0, 0.008, e.z);
  let earw = 1.0 - smoothstep(0.0, 0.01, sd_ear(hm));
  var ec = mix(mix(grey, tawny, 0.6), dark, 0.7 * smoothstep(EAR_H - 0.03, EAR_H, e.y));  // dark tips
  ec = mix(ec, cream, inear);
  hc = mix(hc, ec, earw * smoothstep(0.02, 0.05, q.y));
  c = mix(c, hc, wh);

  // fur streaks
  c = c * (0.85 + 0.15 * n2) * (0.9 + 0.2 * n1);
  return clamp(c, vec3f(0.0), vec3f(1.0));
}
