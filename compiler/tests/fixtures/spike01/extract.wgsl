// Extraction (sketch 02 §§3–4 and pass 3), hand-written as the compiler would emit
// `cull_blocks<GrazerField>`, `place_vertices<GrazerField>` and `emit_quads`.
// Appended to field.wgsl. Nothing here is read back by the CPU: counts stay on the GPU as
// indirect-dispatch and indirect-draw arguments.

struct Grid {
  origin: vec3f,
  cell: f32,
  dims: vec3u,          // blocks per axis; a block is 4×4×4 cells
  nblocks: u32,
  vcap: u32,            // vertex capacity
  icap: u32,            // index capacity
  iters: u32,           // root-finding iterations per crossed edge (sketch 02: 4)
  falloff: f32,         // skin-weight falloff (GRAZER_LOOK: 6cm)
}

struct SkinVertex {     // sketch 02 §2; 48 bytes, also read as a vertex buffer
  pos: vec3f,
  bones: u32,           // [u8; 4]
  nrm: vec3f,
  weights: u32,         // [Unorm8; 4]
  parts: u32,           // PartMask
  pad0: u32, pad1: u32, pad2: u32,
}

struct DrawState {      // the first five words are drawIndexedIndirect's arguments
  index_count: atomic<u32>,
  instance_count: u32,
  first_index: u32,
  base_vertex: u32,
  first_instance: u32,
  vertex_count: atomic<u32>,
  holes: atomic<u32>,   // quads skipped because a neighbouring cell had no vertex
  flags: atomic<u32>,   // 2: vertex capacity hit, 4: index capacity hit
}

@group(0) @binding(0) var<uniform> G: Grazer;
@group(0) @binding(1) var<uniform> grid: Grid;
@group(0) @binding(2) var<storage, read_write> live: array<vec2u>;          // (block, PartMask)
@group(0) @binding(3) var<storage, read_write> live_args: array<atomic<u32>, 3>; // live count = dispatch x
@group(0) @binding(4) var<storage, read_write> block_map: array<u32>;       // block → live slot + 1
@group(0) @binding(5) var<storage, read_write> cells: array<u32>;           // slot·64 + cell → signs<<24 | vertex
@group(0) @binding(6) var<storage, read_write> verts: array<SkinVertex>;
@group(0) @binding(7) var<storage, read_write> out: DrawState;
@group(0) @binding(8) var<storage, read_write> indices: array<u32>;

const NO_VERTEX: u32 = 0xFFFFFFu;

// Lipschitz constants for the intervals (the compiler derives these as facts::lipschitz, D-077).
// Round cones and their intersections are exact or bounds (L = 1). The ellipsoid bound's
// constant is assumed, not derived; the CPU probe checks the whole field's gradient.
const L_ELLIPSOID: f32 = 1.25;

fn block_coords(b: u32) -> vec3u {
  return vec3u(b % grid.dims.x, (b / grid.dims.x) % grid.dims.y, b / (grid.dims.x * grid.dims.y));
}

fn block_index(bc: vec3u) -> u32 {
  return bc.x + grid.dims.x * (bc.y + grid.dims.y * bc.z);
}

// ---- pass 1: which blocks matter, and which parts matter in each (sketch 02 §3) --------------

@compute @workgroup_size(64)
fn cull_blocks(@builtin(global_invocation_id) id: vec3u) {
  let b = id.x;
  if (b >= grid.nblocks) { return; }
  let size = grid.cell * 4.0;
  let center = grid.origin + (vec3f(block_coords(b)) + 0.5) * size;
  let r = length(vec3f(0.5 * size + grid.cell));   // half-diagonal of the block grown by one cell
  let k = G.misc.x;
  let amp = G.misc.y * 1.875;                      // |fbm| ≤ 1 + 1/2 + 1/4 + 1/8

  // Each part's interval over the grown block, from its distance at the centre and its L.
  var lo: array<f32, 20>;
  var hi: array<f32, 20>;
  var min_hi = BIG;
  for (var i = 0u; i < PARTS; i++) {
    var d: f32;
    var spread: f32;
    if (i == 0u) { d = torso_base_d(center); spread = L_ELLIPSOID * r + amp; }
    else if (i == 2u) { d = head_d(center); spread = L_ELLIPSOID * r; }
    else if (i >= 16u) { d = hoof_d(center, i); spread = r; }
    else { d = cone_d(center, i); spread = r; }
    lo[i] = d - spread;
    hi[i] = d + spread;
    min_hi = min(min_hi, hi[i]);
  }

  // PartMask: a part can be dropped where it's at least k above the minimum (smin is then exact).
  var mask = 0u;
  for (var i = 0u; i < PARTS; i++) {
    if (lo[i] < min_hi + k) { mask |= 1u << i; }
  }

  // The creature's interval: smin's interval rule, folded in the same order as the field.
  // Each step subtracts up to k/4, but only where the two intervals come within k.
  var rlo = BIG;
  var rhi = BIG;
  for (var i = 0u; i < PARTS; i++) {
    if (((mask >> i) & 1u) == 0u) { continue; }
    let apart = lo[i] >= rhi + k || rlo >= hi[i] + k;
    rlo = min(rlo, lo[i]) - select(0.25 * k, 0.0, apart);
    rhi = min(rhi, hi[i]);
  }
  if (rlo > 0.0 || rhi < 0.0) { return; }          // no surface in this block

  let slot = atomicAdd(&live_args[0], 1u);
  live[slot] = vec2u(b, mask);
  block_map[b] = slot + 1u;
}

// ---- pass 2: one vertex per crossed cell (sketch 02 §4) --------------------------------------

var<workgroup> corner: array<f32, 125>;            // the block's 5×5×5 corner distances

fn corner_offset(j: u32) -> vec3f {
  return vec3f(f32(j & 1u), f32((j >> 1u) & 1u), f32((j >> 2u) & 1u));
}

fn solve3(a: mat3x3f, b: vec3f) -> vec3f {
  let det = determinant(a);
  if (abs(det) < 1e-12) { return vec3f(0.0); }
  return vec3f(determinant(mat3x3f(b, a[1], a[2])),
               determinant(mat3x3f(a[0], b, a[2])),
               determinant(mat3x3f(a[0], a[1], b))) / det;
}

// Engine code (sketch 02 §4, `skin.weights_at`): spread a part's weight onto its bone(s).
fn add_part_weight(bw: ptr<function, array<f32, 29>>, i: u32, p: vec3f, w: f32) {
  if (i == 0u) {                                   // torso: pelvis → chest along z
    let z0 = pv(0u, 1u).w;
    let z1 = pv(0u, 3u).w;
    let t = clamp((p.z - z0) / (z1 - z0), 0.0, 1.0);
    (*bw)[0] += w * (1.0 - t);
    (*bw)[1] += w * t;
    return;
  }
  if (i == 1u || i == 3u) {                        // neck (bones 2–5) and tail (bones 7–12) chains
    let a = pv(i, 0u).xyz;
    let b = pv(i, 1u).xyz;
    let t = clamp(dot(p - a, b - a) / dot(b - a, b - a), 0.0, 1.0);
    let first = select(7u, 2u, i == 1u);
    let nb = select(6.0, 4.0, i == 1u);
    let u = clamp(t * nb - 0.5, 0.0, nb - 1.0);
    let j = floor(u);
    let fr = u - j;
    let b0 = first + u32(j);
    (*bw)[b0] += w * (1.0 - fr);
    if (fr > 0.0) { (*bw)[b0 + 1u] += w * fr; }
    return;
  }
  if (i == 2u) { (*bw)[6] += w; return; }
  if (i >= 16u) { (*bw)[13u + (i - 16u) * 4u + 3u] += w; return; }
  let leg = (i - 4u) / 3u;
  let seg = (i - 4u) % 3u;
  (*bw)[13u + leg * 4u + seg] += w;
}

@compute @workgroup_size(4, 4, 4)
fn place_vertices(@builtin(workgroup_id) wid: vec3u,
                  @builtin(local_invocation_id) lid: vec3u,
                  @builtin(local_invocation_index) li: u32) {
  let slot = wid.x;
  let lb = live[slot];
  let mask = lb.y;
  let cell = grid.cell;
  let fp = cell;                                   // bandlimit: detail finer than a cell is for shading
  let base = grid.origin + vec3f(block_coords(lb.x) * 4u) * cell;

  for (var i = li; i < 125u; i += 64u) {
    let c = vec3u(i % 5u, (i / 5u) % 5u, i / 25u);
    corner[i] = grazer_d(base + vec3f(c) * cell, mask, fp);
  }
  workgroupBarrier();

  var vals: array<f32, 8>;
  var signs = 0u;
  for (var j = 0u; j < 8u; j++) {
    let c = lid + vec3u(j & 1u, (j >> 1u) & 1u, (j >> 2u) & 1u);
    vals[j] = corner[c.x + c.y * 5u + c.z * 25u];
    if (vals[j] < 0.0) { signs |= 1u << j; }
  }
  let ci = slot * 64u + li;
  if (signs == 0u || signs == 255u) {
    cells[ci] = (signs << 24u) | NO_VERTEX;
    return;
  }

  // Where the surface crosses each of the 12 edges, and its normal there.
  let cmin = base + vec3f(lid) * cell;
  var pts: array<vec3f, 12>;
  var nrm: array<vec3f, 12>;
  var n = 0u;
  var mass = vec3f(0.0);
  for (var e = 0u; e < 12u; e++) {
    let axis = e >> 2u;
    let r = e & 3u;
    let ja = ((r & 1u) << ((axis + 1u) % 3u)) | ((r >> 1u) << ((axis + 2u) % 3u));
    let jb = ja | (1u << axis);
    if (((signs >> ja) & 1u) == ((signs >> jb) & 1u)) { continue; }
    var pa = cmin + corner_offset(ja) * cell;
    var pb = cmin + corner_offset(jb) * cell;
    var fa = vals[ja];
    var fb = vals[jb];
    for (var it = 0u; it < grid.iters; it++) {     // regula falsi
      let x = pa + (pb - pa) * (fa / (fa - fb));
      let fx = grazer_d(x, mask, fp);
      if ((fx < 0.0) == (fa < 0.0)) { pa = x; fa = fx; } else { pb = x; fb = fx; }
    }
    let x = pa + (pb - pa) * (fa / (fa - fb));
    pts[n] = x;
    nrm[n] = normalize(grazer_g(x, mask, fp).g);
    mass += x;
    n++;
  }

  // QEF, regularized toward the mass point, clamped to the cell.
  let m = mass / f32(n);
  let lambda = 0.05;
  var a = mat3x3f(vec3f(lambda, 0.0, 0.0), vec3f(0.0, lambda, 0.0), vec3f(0.0, 0.0, lambda));
  var rhs = vec3f(0.0);
  for (var i = 0u; i < n; i++) {
    let q = nrm[i];
    a += mat3x3f(q * q.x, q * q.y, q * q.z);
    rhs += q * dot(q, pts[i] - m);
  }
  let pos = clamp(m + solve3(a, rhs), cmin, cmin + vec3f(cell));

  // Normal, and skin weights from each part's own distance (D-023).
  let s = grazer_g(pos, mask, fp);
  var pd: array<f32, 20>;
  var dmin = BIG;
  for (var i = 0u; i < PARTS; i++) {
    pd[i] = BIG;
    if (((mask >> i) & 1u) != 0u) {
      pd[i] = part_d(pos, i, fp);
      dmin = min(dmin, pd[i]);
    }
  }
  var bw: array<f32, 29>;
  for (var i = 0u; i < PARTS; i++) {
    if (pd[i] >= BIG) { continue; }
    let w = 1.0 - smoothstep(0.0, grid.falloff, pd[i] - dmin);
    if (w > 0.0) { add_part_weight(&bw, i, pos, w); }
  }
  var idx = vec4u(0u);
  var wq = vec4f(0.0);
  for (var k = 0u; k < 4u; k++) {
    var best = 0u;
    var bv = -1.0;
    for (var b = 0u; b < 29u; b++) {
      if (bw[b] > bv) { bv = bw[b]; best = b; }
    }
    idx[k] = best;
    wq[k] = max(bv, 0.0);
    bw[best] = -1.0;
  }
  wq /= max(wq.x + wq.y + wq.z + wq.w, 1e-9);
  var q = vec4u(round(wq * 255.0));
  q.x = 255u - (q.y + q.z + q.w);

  let vi = atomicAdd(&out.vertex_count, 1u);
  if (vi >= grid.vcap) {
    atomicOr(&out.flags, 2u);
    cells[ci] = (signs << 24u) | NO_VERTEX;
    return;
  }
  verts[vi] = SkinVertex(pos, idx.x | (idx.y << 8u) | (idx.z << 16u) | (idx.w << 24u),
                         normalize(s.g), q.x | (q.y << 8u) | (q.z << 16u) | (q.w << 24u),
                         mask, 0u, 0u, 0u);
  cells[ci] = (signs << 24u) | vi;
}

// ---- pass 3: a quad for every crossed edge, joining the four cells around it ------------------

fn vertex_at(gc: vec3i) -> u32 {
  let dims = vec3i(grid.dims * 4u);
  if (any(gc < vec3i(0)) || any(gc >= dims)) { return NO_VERTEX; }
  let g = vec3u(gc);
  let s1 = block_map[block_index(g / 4u)];
  if (s1 == 0u) { return NO_VERTEX; }
  let l = g % 4u;
  return cells[(s1 - 1u) * 64u + l.x + l.y * 4u + l.z * 16u] & NO_VERTEX;
}

@compute @workgroup_size(4, 4, 4)
fn emit_quads(@builtin(workgroup_id) wid: vec3u,
              @builtin(local_invocation_id) lid: vec3u,
              @builtin(local_invocation_index) li: u32) {
  let slot = wid.x;
  let lb = live[slot];
  let data = cells[slot * 64u + li];
  let v0 = data & NO_VERTEX;
  if (v0 == NO_VERTEX) { return; }
  let signs = data >> 24u;
  let gc = vec3i(block_coords(lb.x) * 4u + lid);
  let in0 = signs & 1u;
  for (var axis = 0u; axis < 3u; axis++) {         // the three edges leaving the cell's min corner
    let in1 = (signs >> (1u << axis)) & 1u;
    if (in0 == in1) { continue; }
    var o1 = vec3i(0);
    var o2 = vec3i(0);
    o1[(axis + 1u) % 3u] = 1;
    o2[(axis + 2u) % 3u] = 1;
    let v1 = vertex_at(gc - o1);
    let v2 = vertex_at(gc - o1 - o2);
    let v3 = vertex_at(gc - o2);
    if (v1 == NO_VERTEX || v2 == NO_VERTEX || v3 == NO_VERTEX) {
      atomicAdd(&out.holes, 1u);
      continue;
    }
    let at = atomicAdd(&out.index_count, 6u);
    if (at + 6u > grid.icap) {
      atomicOr(&out.flags, 4u);
      continue;
    }
    if (in0 == 1u) {                               // inside → outside along +axis: counter-clockwise from outside
      indices[at] = v0; indices[at + 1u] = v1; indices[at + 2u] = v2;
      indices[at + 3u] = v0; indices[at + 4u] = v2; indices[at + 5u] = v3;
    } else {
      indices[at] = v0; indices[at + 1u] = v2; indices[at + 2u] = v1;
      indices[at + 3u] = v0; indices[at + 4u] = v3; indices[at + 5u] = v2;
    }
  }
}
