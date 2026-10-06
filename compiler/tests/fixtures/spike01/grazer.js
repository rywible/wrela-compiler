// What sketch 01's `grazer(seed)` computes, written by hand.
//
// The grazer's *structure* (which parts, which combinators) is its type, so it's fixed in the
// shaders (D-070). Only the numbers below vary per individual, and they reach the GPU as one
// uniform buffer. Nothing here uses Math.sin/cos, so the parameters are the same bits on any
// JS engine (they also feed the CPU determinism check).

export const PARTS = 20;
export const BONES = 29;
export const HEADER_FLOATS = 8;
export const PART_FLOATS = 16;
export const PARAM_FLOATS = HEADER_FLOATS + PARTS * PART_FLOATS; // 328: the `Grazer` uniform

// Part indices. Their order is the fold order of the creature's smooth union.
export const TORSO = 0, NECK = 1, HEAD = 2, TAIL = 3, LEG0 = 4, HOOF0 = 16;

// Bone indices. Legs: 0 fore-left, 1 fore-right, 2 hind-left, 3 hind-right; 4 bones each.
export const PELVIS = 0, CHEST = 1, NECK0 = 2, HEAD_BONE = 6, TAIL0 = 7, LEG_BONE0 = 13;

const add = (a, b) => [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
const scl = (a, s) => [a[0] * s, a[1] * s, a[2] * s];

/** Small deterministic generator (mulberry32). */
export function rng(seed) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The rest skeleton at scale 1, metres. +y up, +z forward. Proportions are the spike's own
 *  (sketch 01's numbers put the hips outside the body), but the bone structure is the sketch's. */
function restSkeleton() {
  const joints = [], parents = [];
  const bone = (p, parent) => { joints.push(p); parents.push(parent); return joints.length - 1; };
  const pelvis = bone([0, 1.30, 0.0], -1);
  const chest = bone([0, 1.30, 1.0], pelvis);
  const neckBase = [0, 1.50, 1.55], neckSpan = [0, 0.55, 0.45];
  let prev = chest;
  for (let i = 0; i < 4; i++) prev = bone(add(neckBase, scl(neckSpan, i / 4)), prev);
  bone(add(add(neckBase, neckSpan), [0, 0, 0.12]), prev); // head
  const tailBase = [0, 1.45, -0.40], tailSpan = [0, -0.55, -0.45];
  prev = pelvis;
  for (let i = 0; i < 6; i++) prev = bone(add(tailBase, scl(tailSpan, i / 6)), prev);
  const legs = [
    { hip: [0.25, 1.15, 1.25], parent: chest, segs: [[0, -0.45, -0.04], [0, -0.42, 0.03], [0, -0.20, 0.02]] },
    { hip: [-0.25, 1.15, 1.25], parent: chest, segs: [[0, -0.45, -0.04], [0, -0.42, 0.03], [0, -0.20, 0.02]] },
    { hip: [0.25, 1.25, 0.05], parent: pelvis, segs: [[0, -0.50, 0.10], [0, -0.45, -0.12], [0, -0.22, 0.0]] },
    { hip: [-0.25, 1.25, 0.05], parent: pelvis, segs: [[0, -0.50, 0.10], [0, -0.45, -0.12], [0, -0.22, 0.0]] },
  ];
  for (const leg of legs) {
    let p = leg.hip;
    prev = bone(p, leg.parent);
    for (const s of leg.segs) { p = add(p, s); prev = bone(p, prev); }
  }
  return { joints, parents };
}

const REST = restSkeleton();
export const PARENTS = REST.parents;

/** One individual: the `Grazer` uniform's contents, rest joints, and bounds. */
export function makeGrazer(seed) {
  const r = rng(seed * 7919 + 17);
  const bulk = 0.9 + 0.25 * r();
  const s = 0.92 + 0.16 * r();
  const joints = REST.joints.map(j => scl(j, s));
  const J = i => joints[i];

  const params = new Float32Array(PARAM_FLOATS);
  // misc: blend k, fbm amplitude, fbm frequency, fbm octaves
  params.set([0.06 * s, 0.003, 25.0, 4.0], 0);
  // seed: noise offset xyz, dapple frequency
  params.set([100 * r(), 100 * r(), 100 * r(), 3.0], 4);

  const part = (i, v0, v1, v2 = [0, 0, 0, 0], v3 = [0, 0, 0, 0]) =>
    params.set([...v0, ...v1, ...v2, ...v3], HEADER_FLOATS + i * PART_FLOATS);
  const C = J(CHEST), P = J(PELVIS);

  // Torso: two ellipsoids, smooth-unioned (k 15cm), then displaced by fbm (in the shader).
  // v1.w and v3.w carry the pelvis and chest z for the skin weights' chest/pelvis split.
  part(TORSO,
    [...add(C, scl([0, 0.05, -0.15], s)), 0.15 * s],
    [...scl([0.42, 0.48, 0.80], s * bulk), P[2]],
    [...add(C, scl([0, 0.05, -0.85], s)), 0],
    [...scl([0.38, 0.44, 0.50], s * bulk), C[2]]);

  // Neck: a round cone along the neck chain's span.
  const neckEnd = add(J(NECK0), scl([0, 0.55, 0.45], s));
  part(NECK, [...J(NECK0), 0.22 * s * bulk], [...neckEnd, 0.12 * s]);

  // Head: an ellipsoid smooth-unioned with a muzzle cone (k 5cm).
  const H = J(HEAD_BONE);
  part(HEAD,
    [...add(H, scl([0, -0.04, 0.10], s)), 0.05 * s],
    [...scl([0.12, 0.15, 0.24], s), 0],
    [...add(H, scl([0, -0.05, 0.15], s)), 0.10 * s],
    [...add(H, scl([0, -0.16, 0.42], s)), 0.065 * s]);

  // Tail: a round cone along the tail chain's span.
  part(TAIL, [...J(TAIL0), 0.07 * s], [...add(J(TAIL0), scl([0, -0.55, -0.45], s)), 0.025 * s]);

  // Legs: three round cones per leg, then a hoof (round cone ∩ half-space).
  const radii = [[0.10 * bulk, 0.065], [0.065, 0.048], [0.048, 0.042]];
  for (let l = 0; l < 4; l++) {
    const b = LEG_BONE0 + l * 4;
    for (let j = 0; j < 3; j++)
      part(LEG0 + l * 3 + j, [...J(b + j), radii[j][0] * s], [...J(b + j + 1), radii[j][1] * s]);
    const foot = J(b + 3);
    part(HOOF0 + l, [...foot, 0.06 * s], [...add(foot, [0, -0.08 * s, 0]), 0.07 * s],
      [foot[1] - 0.07 * s, 0, 0, 0]);
  }

  return { seed, bulk, scale: s, params, joints, bounds: bounds(params) };
}

/** Conservative rest-space bounds from each part's own bounds, padded for the blend. */
function bounds(params) {
  const lo = [Infinity, Infinity, Infinity], hi = [-Infinity, -Infinity, -Infinity];
  const grow = (c, r) => { for (let a = 0; a < 3; a++) { lo[a] = Math.min(lo[a], c[a] - r[a]); hi[a] = Math.max(hi[a], c[a] + r[a]); } };
  const v = (i, k) => Array.from(params.subarray(HEADER_FLOATS + i * PART_FLOATS + k * 4, HEADER_FLOATS + i * PART_FLOATS + k * 4 + 4));
  const amp = params[1] * 1.875;
  grow(v(TORSO, 0), v(TORSO, 1).slice(0, 3).map(x => x + amp));
  grow(v(TORSO, 2), v(TORSO, 3).slice(0, 3).map(x => x + amp));
  grow(v(HEAD, 0), v(HEAD, 1).slice(0, 3));
  const cone = (a, b) => { const r = Math.max(a[3], b[3]); grow(a, [r, r, r]); grow(b, [r, r, r]); };
  cone(v(HEAD, 2), v(HEAD, 3));
  for (const i of [NECK, TAIL]) cone(v(i, 0), v(i, 1));
  for (let i = LEG0; i < HOOF0 + 4; i++) cone(v(i, 0), v(i, 1));
  const pad = params[0] * 0.25;
  return { min: lo.map(x => x - pad), max: hi.map(x => x + pad) };
}

// ---- Presentation: a procedural walk cycle (stands in for the gait controller) ----------------

export const mat4 = {
  identity: () => { const m = new Float32Array(16); m[0] = m[5] = m[10] = m[15] = 1; return m; },
  mul(a, b, out = new Float32Array(16)) {
    for (let c = 0; c < 4; c++) for (let r = 0; r < 4; r++) {
      out[c * 4 + r] = a[r] * b[c * 4] + a[4 + r] * b[c * 4 + 1] + a[8 + r] * b[c * 4 + 2] + a[12 + r] * b[c * 4 + 3];
    }
    return out;
  },
  translate(x, y, z) { const m = mat4.identity(); m[12] = x; m[13] = y; m[14] = z; return m; },
  rotX(t) { const m = mat4.identity(), c = Math.cos(t), s = Math.sin(t); m[5] = c; m[6] = s; m[9] = -s; m[10] = c; return m; },
  rotY(t) { const m = mat4.identity(), c = Math.cos(t), s = Math.sin(t); m[0] = c; m[2] = -s; m[8] = s; m[10] = c; return m; },
  rotZ(t) { const m = mat4.identity(), c = Math.cos(t), s = Math.sin(t); m[0] = c; m[1] = s; m[4] = -s; m[5] = c; return m; },
  perspective(fovy, aspect, near, far) {
    const f = 1 / Math.tan(fovy / 2), m = new Float32Array(16);
    m[0] = f / aspect; m[5] = f; m[10] = far / (near - far); m[11] = -1; m[14] = near * far / (near - far);
    return m;
  },
  ortho(l, r, b, t, n, f) {
    const m = mat4.identity();
    m[0] = 2 / (r - l); m[5] = 2 / (t - b); m[10] = 1 / (n - f);
    m[12] = -(r + l) / (r - l); m[13] = -(t + b) / (t - b); m[14] = n / (n - f);
    return m;
  },
  lookAt(eye, target, up) {
    const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    const norm = a => { const l = Math.hypot(...a); return a.map(x => x / l); };
    const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    const z = norm(sub(eye, target)), x = norm(cross(up, z)), y = cross(z, x);
    return new Float32Array([x[0], y[0], z[0], 0, x[1], y[1], z[1], 0, x[2], y[2], z[2], 0,
      -dot(x, eye), -dot(y, eye), -dot(z, eye), 1]);
  },
};

/** Writes the bone palette (BONES mat4s, rest space → world) for one grazer at time t.
 *  `inst` = { x, y, z, yaw, phase, headDown }. */
export function pose(g, inst, t, out, offset) {
  const { rotX, rotY, rotZ, translate, mul } = mat4;
  const phi = inst.phase + t * 0.9;
  const tau = 2 * Math.PI;
  const local = new Array(BONES);
  for (let i = 0; i < BONES; i++) local[i] = null;

  local[PELVIS] = mul(rotX(0.02 * Math.sin(2 * tau * phi)), rotZ(0.03 * Math.sin(tau * phi)));
  for (let i = 0; i < 4; i++) local[NECK0 + i] = rotX(inst.headDown * 0.22 + 0.04 * Math.sin(2 * tau * phi + i * 0.5));
  local[HEAD_BONE] = rotX(-inst.headDown * 0.3);
  for (let i = 0; i < 6; i++) local[TAIL0 + i] = mul(rotY(0.15 * Math.sin(tau * phi - i * 0.6) * (i + 1) / 6), rotX(0.05 * i));
  const offsets = [0.25, 0.75, 0.0, 0.5]; // lateral-sequence walk
  for (let l = 0; l < 4; l++) {
    const c = tau * (phi + offsets[l]);
    const flex = Math.max(0, -Math.cos(c));
    const fore = l < 2 ? 1 : -1;
    const b = LEG_BONE0 + l * 4;
    local[b] = rotX(-0.30 * Math.sin(c));
    local[b + 1] = rotX(0.45 * flex * fore);
    local[b + 2] = rotX(-0.5 * flex * fore);
    local[b + 3] = rotX(-0.2 * flex);
  }

  const root = mul(translate(inst.x, inst.y - 0.025 * Math.cos(2 * tau * phi), inst.z), rotY(inst.yaw));
  const world = new Array(BONES);
  const J = g.joints;
  for (let i = 0; i < BONES; i++) {
    const p = PARENTS[i];
    const base = p < 0 ? mul(root, translate(...J[i]))
      : mul(world[p], translate(J[i][0] - J[p][0], J[i][1] - J[p][1], J[i][2] - J[p][2]));
    world[i] = local[i] ? mul(base, local[i]) : base;
    out.set(mul(world[i], translate(-J[i][0], -J[i][1], -J[i][2])), offset + i * 16);
  }
}

/** Terrain height for placing grazers. Must match `terrain_h` in draw.wgsl. */
export function terrainHeight(x, z) {
  return 0.6 * Math.sin(0.11 * x) * Math.cos(0.09 * z)
    + 0.3 * Math.sin(0.23 * x + 1.3) * Math.sin(0.19 * z + 0.4)
    + 0.08 * Math.sin(0.9 * x) * Math.cos(0.7 * z);
}
