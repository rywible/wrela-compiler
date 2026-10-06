// Spike harness: builds the pipelines, extracts the herd, renders the scenes, times all of it.
// This file plays the engine's role (sketch 02 §6) plus the measuring. It isn't compiler output.

import { makeGrazer, pose, mat4, terrainHeight, rng, PARAM_FLOATS } from './grazer.js';

const W = 1920, H = 1080, HERD = 40;
const TERRAIN_VERTS = 256 * 256 * 6;
const PALETTE_STRIDE = 2048;        // 29 mat4 = 1856 bytes, padded to the storage offset alignment
const ITERS = 4, FALLOFF = 0.06;    // sketch 02's root iterations; GRAZER_LOOK's 6cm skin falloff
const U = GPUBufferUsage;

const $ = id => document.getElementById(id);
const log = (...a) => { $('log').textContent += a.join(' ') + '\n'; console.log(...a); };
const R = { started: new Date().toISOString(), device: {}, cpu: {}, pipelines: {}, extraction: {}, frames: {} };
window.__results = R;

let device, ctx, canvasFormat;
const src = {}, L = {}, P = {}, T = {}, B = {};

const median = a => { const s = [...a].sort((x, y) => x - y); return s.length ? s[s.length >> 1] : NaN; };
const r3 = x => Math.round(x * 1000) / 1000;

function buffer(size, usage, data) {
  const b = device.createBuffer({ size: Math.max(16, Math.ceil(size / 16) * 16), usage, mappedAtCreation: !!data });
  if (data) { new data.constructor(b.getMappedRange()).set(data); b.unmap(); }
  return b;
}

async function readback(copies) {
  const total = copies.reduce((s, c) => s + c.size, 0);
  const rb = device.createBuffer({ size: total, usage: U.MAP_READ | U.COPY_DST });
  const enc = device.createCommandEncoder();
  let off = 0;
  for (const c of copies) { enc.copyBufferToBuffer(c.src, 0, rb, off, c.size); off += c.size; }
  device.queue.submit([enc.finish()]);
  await rb.mapAsync(GPUMapMode.READ);
  const out = rb.getMappedRange().slice(0);
  rb.unmap();
  rb.destroy();
  return out;
}

async function save(name, body) {
  try { await fetch(`results/${name}`, { method: 'PUT', body }); } catch (e) { log('save failed', name, e.message); }
}

// ---- setup -----------------------------------------------------------------------------------

async function init() {
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'high-performance' });
  const want = ['timestamp-query'].filter(f => adapter.features.has(f));
  device = await adapter.requestDevice({
    requiredFeatures: want,
    requiredLimits: {
      maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize,
      maxBufferSize: adapter.limits.maxBufferSize,
    },
  });
  device.lost.then(i => log('DEVICE LOST:', i.message));
  device.addEventListener('uncapturederror', e => log('GPU ERROR:', e.error.message));
  const info = adapter.info || {};
  R.device = {
    vendor: info.vendor, architecture: info.architecture, description: info.description,
    userAgent: navigator.userAgent, timestampQuery: want.includes('timestamp-query'),
    hardwareConcurrency: navigator.hardwareConcurrency,
    pageVisibility: document.visibilityState,
  };
  if (!R.device.timestampQuery) throw new Error('timestamp-query is required');
  for (const f of ['field', 'extract', 'draw', 'blit', 'count']) src[f] = await (await fetch(`${f}.wgsl`)).text();

  const canvas = $('view');
  canvas.width = W;
  canvas.height = H;
  ctx = canvas.getContext('webgpu');
  canvasFormat = navigator.gpu.getPreferredCanvasFormat();
  ctx.configure({ device, format: canvasFormat, alphaMode: 'opaque' });

  const C = GPUShaderStage.COMPUTE, VS = GPUShaderStage.VERTEX, FS = GPUShaderStage.FRAGMENT;
  const e = (binding, visibility, type) => ({ binding, visibility, buffer: { type } });
  const extract = skip => device.createBindGroupLayout({
    entries: [e(0, C, 'uniform'), e(1, C, 'uniform'),
      ...[2, 3, 4, 5, 6, 7, 8].filter(b => b !== skip).map(b => e(b, C, 'storage'))],
  });
  L.cull = extract(-1);
  L.place = extract(3);   // live_args is the indirect buffer here, so it can't also be bound
  L.frame = device.createBindGroupLayout({
    entries: [
      e(0, VS | FS, 'uniform'),
      { binding: 1, visibility: FS, texture: { sampleType: 'depth' } },
      { binding: 2, visibility: FS, sampler: { type: 'comparison' } },
      { binding: 3, visibility: FS, texture: { sampleType: 'float', viewDimension: '3d' } },
      { binding: 4, visibility: FS, sampler: { type: 'filtering' } },
    ],
  });
  L.shadowFrame = device.createBindGroupLayout({ entries: [e(0, VS | FS, 'uniform')] });
  L.grazer = device.createBindGroupLayout({ entries: [e(0, FS, 'uniform'), e(1, VS, 'read-only-storage')] });
  L.count = device.createBindGroupLayout({
    entries: [{ binding: 0, visibility: C, texture: { sampleType: 'unfilterable-float' } }, e(1, C, 'storage')],
  });
  L.blit = device.createBindGroupLayout({
    entries: [{ binding: 0, visibility: FS, texture: {} }, { binding: 1, visibility: FS, sampler: {} }],
  });
}

const salted = (code, salt) => salt == null ? code : code.replace('const SALT: f32 = 0.0;', `const SALT: f32 = ${salt.toFixed(4)};`);

async function checkModule(module, label) {
  const info = await module.getCompilationInfo();
  for (const m of info.messages) log(`${label} ${m.type} line ${m.lineNum}:${m.linePos} ${m.message}`);
  if (info.messages.some(m => m.type === 'error')) throw new Error(`${label}: WGSL errors`);
}

/** Creates every pipeline the scene needs and times it. `salt` makes the source unique, so no
 *  cache can serve it; `parallel` creates them all at once, as a loading screen would. */
async function pipelines(salt, parallel) {
  const t0 = performance.now();
  const em = device.createShaderModule({ code: salted(src.field, salt) + src.extract, label: 'extract' });
  const dm = device.createShaderModule({ code: salted(src.field, salt) + src.draw, label: 'draw' });
  const vbuf = [{
    arrayStride: 48,
    attributes: [
      { shaderLocation: 0, offset: 0, format: 'float32x3' },
      { shaderLocation: 1, offset: 12, format: 'uint8x4' },
      { shaderLocation: 2, offset: 16, format: 'float32x3' },
      { shaderLocation: 3, offset: 28, format: 'unorm8x4' },
      { shaderLocation: 4, offset: 32, format: 'uint32' },
    ],
  }];
  const pl = (...bgls) => device.createPipelineLayout({ bindGroupLayouts: bgls });
  const compute = (entryPoint, bgl) => () => device.createComputePipelineAsync({ layout: pl(bgl), compute: { module: em, entryPoint } });
  const prim = { topology: 'triangle-list', cullMode: 'back', frontFace: 'ccw' };
  const depth = (depthCompare, depthWriteEnabled) => ({ format: 'depth32float', depthCompare, depthWriteEnabled });
  const shade = (entryPoint, prepass) => () => device.createRenderPipelineAsync({
    layout: pl(L.frame, L.grazer),
    vertex: { module: dm, entryPoint: 'skin', buffers: vbuf },
    fragment: { module: dm, entryPoint, targets: [{ format: 'rgba8unorm-srgb' }] },
    primitive: prim,
    depthStencil: prepass ? depth('equal', false) : depth('less', true),
  });
  const list = [
    ['cull_blocks', compute('cull_blocks', L.cull)],
    ['place_vertices', compute('place_vertices', L.place)],
    ['emit_quads', compute('emit_quads', L.place)],
    ['shadow', () => device.createRenderPipelineAsync({
      layout: pl(L.shadowFrame, L.grazer),
      vertex: { module: dm, entryPoint: 'skin_shadow', buffers: vbuf },
      primitive: { ...prim, cullMode: 'none' },
      depthStencil: { ...depth('less', true), depthBias: 4, depthBiasSlopeScale: 2 },
    })],
    ['prepass', () => device.createRenderPipelineAsync({
      layout: pl(L.frame, L.grazer),
      vertex: { module: dm, entryPoint: 'skin', buffers: vbuf },
      primitive: prim, depthStencil: depth('less', true),
    })],
    ['shade_field', shade('shade_field', true)],
    ['shade_lookup', shade('shade_lookup', true)],
    ['shade_vertex', shade('shade_vertex', true)],
    ['shade_field_np', shade('shade_field', false)],
    ['shade_lookup_np', shade('shade_lookup', false)],
    ['shade_vertex_np', shade('shade_vertex', false)],
    ['terrain', () => device.createRenderPipelineAsync({
      layout: pl(L.frame),
      vertex: { module: dm, entryPoint: 'terrain_vs' },
      fragment: { module: dm, entryPoint: 'terrain_fs', targets: [{ format: 'rgba8unorm-srgb' }] },
      primitive: { topology: 'triangle-list', cullMode: 'none' }, depthStencil: depth('less', true),
    })],
  ];
  const out = {}, ms = {};
  try {
    if (parallel) {
      const ps = await Promise.all(list.map(([, f]) => f()));
      list.forEach(([n], i) => { out[n] = ps[i]; });
    } else {
      for (const [n, f] of list) { const t = performance.now(); out[n] = await f(); ms[n] = r3(performance.now() - t); }
    }
  } catch (err) {
    await checkModule(em, 'extract.wgsl');
    await checkModule(dm, 'draw.wgsl');
    throw err;
  }
  ms.total = r3(performance.now() - t0);
  return { out, ms };
}

/** Pipelines the harness needs that aren't part of the measured scene: blit, coverage, debug. */
async function toolPipelines() {
  const dm = device.createShaderModule({ code: src.field + src.draw, label: 'draw-tools' });
  const bm = device.createShaderModule({ code: src.blit, label: 'blit' });
  const cm = device.createShaderModule({ code: src.count, label: 'count' });
  const vbuf = [{
    arrayStride: 48,
    attributes: [
      { shaderLocation: 0, offset: 0, format: 'float32x3' },
      { shaderLocation: 1, offset: 12, format: 'uint8x4' },
      { shaderLocation: 2, offset: 16, format: 'float32x3' },
      { shaderLocation: 3, offset: 28, format: 'unorm8x4' },
      { shaderLocation: 4, offset: 32, format: 'uint32' },
    ],
  }];
  const layout = device.createPipelineLayout({ bindGroupLayouts: [L.frame, L.grazer] });
  const prim = { topology: 'triangle-list', cullMode: 'back', frontFace: 'ccw' };
  [P.blit, P.coverage, P.shade_debug, P.count] = await Promise.all([
    device.createRenderPipelineAsync({
      layout: device.createPipelineLayout({ bindGroupLayouts: [L.blit] }),
      vertex: { module: bm, entryPoint: 'blit_vs' },
      fragment: { module: bm, entryPoint: 'blit_fs', targets: [{ format: canvasFormat }] },
    }),
    device.createRenderPipelineAsync({
      layout, vertex: { module: dm, entryPoint: 'skin', buffers: vbuf },
      fragment: { module: dm, entryPoint: 'coverage', targets: [{ format: 'r8unorm' }] },
      primitive: prim, depthStencil: { format: 'depth32float', depthCompare: 'equal', depthWriteEnabled: false },
    }),
    device.createRenderPipelineAsync({
      layout, vertex: { module: dm, entryPoint: 'skin', buffers: vbuf },
      fragment: { module: dm, entryPoint: 'shade_debug', targets: [{ format: 'rgba8unorm-srgb' }] },
      primitive: prim, depthStencil: { format: 'depth32float', depthCompare: 'equal', depthWriteEnabled: false },
    }),
    device.createComputePipelineAsync({
      layout: device.createPipelineLayout({ bindGroupLayouts: [L.count] }),
      compute: { module: cm, entryPoint: 'count' },
    }),
  ]);
}

function frameResources() {
  T.color = device.createTexture({ size: [W, H], format: 'rgba8unorm-srgb', usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING });
  T.depth = device.createTexture({ size: [W, H], format: 'depth32float', usage: GPUTextureUsage.RENDER_ATTACHMENT });
  T.shadow = device.createTexture({ size: [2048, 2048], format: 'depth32float', usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING });
  T.coverage = device.createTexture({ size: [W, H], format: 'r8unorm', usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.TEXTURE_BINDING });
  T.coverageView = T.coverage.createView();
  B.count = buffer(16, U.STORAGE | U.COPY_SRC | U.COPY_DST);
  B.countBG = device.createBindGroup({ layout: L.count, entries: [{ binding: 0, resource: T.coverageView }, { binding: 1, resource: { buffer: B.count } }] });
  T.colorView = T.color.createView();
  T.depthView = T.depth.createView();
  T.shadowView = T.shadow.createView();
  T.detail = detailTexture();
  B.view = buffer(160, U.UNIFORM | U.COPY_DST);
  B.palette = buffer(HERD * PALETTE_STRIDE, U.STORAGE | U.COPY_DST);
  const shadowSampler = device.createSampler({ compare: 'less', magFilter: 'linear', minFilter: 'linear' });
  const detailSampler = device.createSampler({
    magFilter: 'linear', minFilter: 'linear', mipmapFilter: 'linear',
    addressModeU: 'repeat', addressModeV: 'repeat', addressModeW: 'repeat',
  });
  B.frame = device.createBindGroup({
    layout: L.frame,
    entries: [
      { binding: 0, resource: { buffer: B.view } },
      { binding: 1, resource: T.shadowView },
      { binding: 2, resource: shadowSampler },
      { binding: 3, resource: T.detail.createView() },
      { binding: 4, resource: detailSampler },
    ],
  });
  B.shadowFrame = device.createBindGroup({ layout: L.shadowFrame, entries: [{ binding: 0, resource: { buffer: B.view } }] });
  B.blit = device.createBindGroup({
    layout: L.blit,
    entries: [{ binding: 0, resource: T.colorView }, { binding: 1, resource: device.createSampler({ magFilter: 'linear', minFilter: 'linear' }) }],
  });
}

/** 64³ RGBA8 with a full mip chain: the lookup baseline's "baked" textures. */
function detailTexture() {
  const N = 64, levels = 7;
  const tex = device.createTexture({
    size: [N, N, N], dimension: '3d', format: 'rgba8unorm', mipLevelCount: levels,
    usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
  });
  const r = rng(99);
  let data = new Uint8Array(N * N * N * 4).map(() => (r() * 256) | 0);
  let n = N;
  for (let l = 0; l < levels; l++) {
    device.queue.writeTexture({ texture: tex, mipLevel: l }, data, { bytesPerRow: n * 4, rowsPerImage: n }, [n, n, n]);
    if (n === 1) break;
    const m = n >> 1, next = new Uint8Array(m * m * m * 4);
    for (let z = 0; z < m; z++) for (let y = 0; y < m; y++) for (let x = 0; x < m; x++) for (let c = 0; c < 4; c++) {
      let s = 0;
      for (let k = 0; k < 8; k++) s += data[((((2 * z + (k >> 2)) * n + 2 * y + ((k >> 1) & 1)) * n) + 2 * x + (k & 1)) * 4 + c];
      next[((z * m + y) * m + x) * 4 + c] = s >> 3;
    }
    data = next;
    n = m;
  }
  return tex;
}

// ---- extraction (sketch 02 §6: realize_mesh) -----------------------------------------------------

function gridFor(g, cell) {
  const pad = 2 * cell;
  const origin = g.bounds.min.map(x => x - pad);
  const dims = [0, 1, 2].map(a => Math.ceil((g.bounds.max[a] + pad - origin[a]) / (4 * cell)));
  return { origin, cell, dims, nblocks: dims[0] * dims[1] * dims[2] };
}

function scratch(maxBlocks, count) {
  return {
    live: buffer(maxBlocks * 8, U.STORAGE),
    liveArgs: buffer(12, U.STORAGE | U.INDIRECT | U.COPY_DST | U.COPY_SRC),
    liveInit: buffer(12, U.COPY_SRC, new Uint32Array([0, 1, 1])),
    blockMap: buffer(maxBlocks * 4, U.STORAGE | U.COPY_DST),
    cells: buffer(maxBlocks * 64 * 4, U.STORAGE),
    drawInit: buffer(32, U.COPY_SRC, new Uint32Array([0, 1, 0, 0, 0, 0, 0, 0])),
    stats: buffer(count * 16, U.COPY_DST | U.COPY_SRC),
    destroy() { for (const k of ['live', 'liveArgs', 'liveInit', 'blockMap', 'cells', 'drawInit', 'stats']) this[k].destroy(); },
  };
}

function individual(g, cell, vcap, icap, ex) {
  const grid = gridFor(g, cell);
  const raw = new ArrayBuffer(48), f = new Float32Array(raw), u = new Uint32Array(raw);
  f.set(grid.origin, 0); f[3] = cell;
  u.set(grid.dims, 4); u[7] = grid.nblocks;
  u[8] = vcap; u[9] = icap; u[10] = ITERS; f[11] = FALLOFF;
  const ind = {
    g, grid, vcap, icap,
    params: buffer(g.params.byteLength, U.UNIFORM, g.params),
    gridBuf: buffer(48, U.UNIFORM, u),
    verts: buffer(vcap * 48, U.STORAGE | U.VERTEX),
    indices: buffer(icap * 4, U.STORAGE | U.INDEX),
    draw: buffer(32, U.STORAGE | U.INDIRECT | U.COPY_DST | U.COPY_SRC),
  };
  const all = [ind.params, ind.gridBuf, ex.live, ex.liveArgs, ex.blockMap, ex.cells, ind.verts, ind.draw, ind.indices];
  const entries = skip => all.map((b, binding) => ({ binding, resource: { buffer: b } })).filter(e => e.binding !== skip);
  ind.cullBG = device.createBindGroup({ layout: L.cull, entries: entries(-1) });
  ind.placeBG = device.createBindGroup({ layout: L.place, entries: entries(3) });
  ind.destroy = () => [ind.params, ind.gridBuf, ind.verts, ind.indices, ind.draw].forEach(b => b.destroy());
  return ind;
}

function encodeExtract(enc, ex, ind, qs, q0, statIndex) {
  enc.clearBuffer(ex.blockMap, 0, ind.grid.nblocks * 4);
  enc.copyBufferToBuffer(ex.liveInit, 0, ex.liveArgs, 0, 12);
  enc.copyBufferToBuffer(ex.drawInit, 0, ind.draw, 0, 32);
  const pass = (i, pipe, bg, dispatch) => {
    const p = enc.beginComputePass(qs ? { timestampWrites: { querySet: qs, beginningOfPassWriteIndex: q0 + 2 * i, endOfPassWriteIndex: q0 + 2 * i + 1 } } : {});
    p.setPipeline(pipe);
    p.setBindGroup(0, bg);
    dispatch(p);
    p.end();
  };
  pass(0, P.cull_blocks, ind.cullBG, p => p.dispatchWorkgroups(Math.ceil(ind.grid.nblocks / 64)));
  pass(1, P.place_vertices, ind.placeBG, p => p.dispatchWorkgroupsIndirect(ex.liveArgs, 0));
  pass(2, P.emit_quads, ind.placeBG, p => p.dispatchWorkgroupsIndirect(ex.liveArgs, 0));
  if (statIndex != null) enc.copyBufferToBuffer(ex.liveArgs, 0, ex.stats, statIndex * 16, 12);
}

/** Extracts `count` individuals at one cell size, three times; reports the last run. */
async function extractHerd(cell, count, keep) {
  const gs = Array.from({ length: count }, (_, i) => makeGrazer(i + 1));
  const grids = gs.map(g => gridFor(g, cell));
  const maxBlocks = Math.max(...grids.map(g => g.nblocks));
  const ex = scratch(maxBlocks, count);

  // Capacity: extract the largest individual once with generous buffers, then size everyone by it.
  const largest = gs[grids.findIndex(g => g.nblocks === maxBlocks)];
  const calCap = Math.min(Math.ceil(maxBlocks * 64 / 8), 2_000_000);
  const cal = individual(largest, cell, calCap, calCap * 6, ex);
  let enc = device.createCommandEncoder();
  encodeExtract(enc, ex, cal, null, 0, null);
  device.queue.submit([enc.finish()]);
  const c = new Uint32Array(await readback([{ src: cal.draw, size: 32 }]));
  cal.destroy();
  const vcap = Math.ceil(c[5] * 1.25) + 1024, icap = Math.ceil(c[0] * 1.25) + 6144;

  const inds = gs.map(g => individual(g, cell, vcap, icap, ex));
  const qs = device.createQuerySet({ type: 'timestamp', count: count * 6 });
  const resolve = buffer(count * 48, U.QUERY_RESOLVE | U.COPY_SRC);
  let last;
  const walls = [];
  for (let run = 0; run < 3; run++) {
    enc = device.createCommandEncoder();
    inds.forEach((ind, i) => encodeExtract(enc, ex, ind, qs, i * 6, i));
    enc.resolveQuerySet(qs, 0, count * 6, resolve, 0);
    const t0 = performance.now();
    device.queue.submit([enc.finish()]);
    await device.queue.onSubmittedWorkDone();
    walls.push(performance.now() - t0);
    last = await readback([{ src: resolve, size: count * 48 }, ...inds.map(ind => ({ src: ind.draw, size: 32 })), { src: ex.stats, size: count * 16 }]);
  }
  qs.destroy();
  resolve.destroy();
  const ts = new BigUint64Array(last, 0, count * 6);
  const draws = new Uint32Array(last, count * 48, count * 8);
  const stats = new Uint32Array(last, count * 48 + count * 32, count * 4);
  const ms = (i, k) => Number(ts[i * 6 + 2 * k + 1] - ts[i * 6 + 2 * k]) / 1e6;
  const per = inds.map((ind, i) => ({
    cull: ms(i, 0), place: ms(i, 1), emit: ms(i, 2), total: ms(i, 0) + ms(i, 1) + ms(i, 2),
    blocks: ind.grid.nblocks, live: stats[i * 4], verts: draws[i * 8 + 5], tris: draws[i * 8] / 3,
    holes: draws[i * 8 + 6], flags: draws[i * 8 + 7],
  }));
  const sum = k => per.reduce((s, p) => s + p[k], 0);
  const res = {
    cell_m: cell, individuals: count,
    gpu_ms_per_individual: { median: r3(median(per.map(p => p.total))), cull: r3(median(per.map(p => p.cull))), place: r3(median(per.map(p => p.place))), emit: r3(median(per.map(p => p.emit))) },
    gpu_ms_herd_total: r3(sum('total')),
    wall_ms_herd_batch: walls.map(r3),
    blocks_median: median(per.map(p => p.blocks)),
    live_blocks_median: median(per.map(p => p.live)),
    live_fraction: r3(sum('live') / sum('blocks')),
    cells_evaluated_median: median(per.map(p => p.live)) * 64,
    verts_median: median(per.map(p => p.verts)),
    tris_median: median(per.map(p => p.tris)),
    tris_herd: sum('tris'),
    holes_total: sum('holes'),
    overflow_flags: per.filter(p => p.flags).length,
    mesh_mb_herd: r3(per.reduce((s, p) => s + p.verts * 48 + p.tris * 12, 0) / 2 ** 20),
    allocated_mb_herd: r3(count * (vcap * 48 + icap * 4) / 2 ** 20),
    max_live_dispatch: Math.max(...per.map(p => p.live)),
  };
  inds.forEach((ind, i) => { ind.tris = per[i].tris; });
  ex.destroy();
  if (!keep) inds.forEach(i => i.destroy());
  return { res, inds };
}

// ---- drawing ---------------------------------------------------------------------------------------

function herdInstances() {
  const r = rng(4242), out = [];
  for (let i = 0; i < HERD; i++) {
    const row = Math.floor(i / 8), col = i % 8;
    const x = (col - 3.5) * 4.0 + (r() - 0.5) * 1.6;
    const z = (row - 2) * 4.6 + (r() - 0.5) * 1.6;
    out.push({ x, z, y: terrainHeight(x, z), yaw: Math.PI / 2 + (r() - 0.5) * 0.7, phase: r(), headDown: r() < 0.4 ? 1 : 0.2 * r() });
  }
  return out;
}

function scene(name, inds, inst, eye, target, shadowExtent) {
  const sun = (() => { const v = [-0.5, 0.75, -0.45], l = Math.hypot(...v); return v.map(x => x / l); })();
  const center = [target[0], 0, target[2]];
  const lightEye = center.map((c, i) => c + sun[i] * 60);
  const light = mat4.mul(mat4.ortho(-shadowExtent, shadowExtent, -shadowExtent, shadowExtent, 1, 140), mat4.lookAt(lightEye, center, [0, 1, 0]));
  const viewproj = mat4.mul(mat4.perspective(40 * Math.PI / 180, W / H, 0.1, 400), mat4.lookAt(eye, target, [0, 1, 0]));
  const draws = inds.map((ind, slot) => ({
    ind,
    bg: device.createBindGroup({
      layout: L.grazer,
      entries: [
        { binding: 0, resource: { buffer: ind.params } },
        { binding: 1, resource: { buffer: B.palette, offset: slot * PALETTE_STRIDE, size: 29 * 64 } },
      ],
    }),
  }));
  return { name, inds, inst, draws, eye, sun, light, viewproj, tris: inds.reduce((t, i) => t + i.tris, 0) };
}

function update(sc, t) {
  const v = new Float32Array(40);
  v.set(sc.viewproj, 0);
  v.set(sc.light, 16);
  v.set([...sc.eye, t], 32);
  v.set([...sc.sun, 0], 36);
  device.queue.writeBuffer(B.view, 0, v);
  const pal = new Float32Array(sc.inds.length * PALETTE_STRIDE / 4);
  sc.inds.forEach((ind, i) => pose(ind.g, sc.inst[i], t, pal, i * PALETTE_STRIDE / 4));
  device.queue.writeBuffer(B.palette, 0, pal);
}

function encodeFrame(enc, sc, mode, prepass, qs, q0) {
  const ts = i => qs ? { timestampWrites: { querySet: qs, beginningOfPassWriteIndex: q0 + 2 * i, endOfPassWriteIndex: q0 + 2 * i + 1 } } : {};
  const drawAll = p => {
    for (const d of sc.draws) {
      p.setBindGroup(1, d.bg);
      p.setVertexBuffer(0, d.ind.verts);
      p.setIndexBuffer(d.ind.indices, 'uint32');
      p.drawIndexedIndirect(d.ind.draw, 0);
    }
  };
  let p = enc.beginRenderPass({ colorAttachments: [], depthStencilAttachment: { view: T.shadowView, depthClearValue: 1, depthLoadOp: 'clear', depthStoreOp: 'store' }, ...ts(0) });
  p.setPipeline(P.shadow);
  p.setBindGroup(0, B.shadowFrame);
  drawAll(p);
  p.end();

  p = enc.beginRenderPass({
    colorAttachments: [{ view: T.colorView, clearValue: [0.42, 0.55, 0.75, 1], loadOp: 'clear', storeOp: 'store' }],
    depthStencilAttachment: { view: T.depthView, depthClearValue: 1, depthLoadOp: 'clear', depthStoreOp: 'store' }, ...ts(1),
  });
  p.setPipeline(P.terrain);
  p.setBindGroup(0, B.frame);
  p.draw(TERRAIN_VERTS);
  p.end();

  if (prepass) {
    p = enc.beginRenderPass({ colorAttachments: [], depthStencilAttachment: { view: T.depthView, depthLoadOp: 'load', depthStoreOp: 'store' }, ...ts(2) });
    p.setPipeline(P.prepass);
    p.setBindGroup(0, B.frame);
    drawAll(p);
    p.end();
  }

  p = enc.beginRenderPass({
    colorAttachments: [{ view: T.colorView, loadOp: 'load', storeOp: 'store' }],
    depthStencilAttachment: { view: T.depthView, depthLoadOp: 'load', depthStoreOp: 'store' }, ...ts(3),
  });
  p.setPipeline(P[`shade_${mode}${prepass ? '' : '_np'}`]);
  p.setBindGroup(0, B.frame);
  drawAll(p);
  p.end();
}

function present() {
  const enc = device.createCommandEncoder();
  const p = enc.beginRenderPass({ colorAttachments: [{ view: ctx.getCurrentTexture().createView(), loadOp: 'clear', storeOp: 'store', clearValue: [0, 0, 0, 1] }] });
  p.setPipeline(P.blit);
  p.setBindGroup(0, B.blit);
  p.draw(3);
  p.end();
  device.queue.submit([enc.finish()]);
}

/** GPU time per pass under sustained load: 30 untimed frames to bring the GPU to a steady clock,
 *  then `frames` frames back to back, each with its own timestamps. Medians are reported. */
async function measure(sc, mode, prepass, frames = 90) {
  const Q = 8;
  const qs = device.createQuerySet({ type: 'timestamp', count: frames * Q });
  for (let f = 0; f < 30; f++) {
    update(sc, f / 60);
    const enc = device.createCommandEncoder();
    encodeFrame(enc, sc, mode, prepass, null, 0);
    device.queue.submit([enc.finish()]);
  }
  await device.queue.onSubmittedWorkDone();
  const t1 = performance.now();
  for (let f = 0; f < frames; f++) {
    update(sc, f / 60);
    const enc = device.createCommandEncoder();
    encodeFrame(enc, sc, mode, prepass, qs, f * Q);
    device.queue.submit([enc.finish()]);
  }
  await device.queue.onSubmittedWorkDone();
  const throughput = (performance.now() - t1) / frames;
  const ts = await resolveTimestamps(qs, frames * Q);
  const d = (f, i) => Number(ts[f * Q + 2 * i + 1] - ts[f * Q + 2 * i]) / 1e6;
  const all = [...Array(frames).keys()];
  const m = fn => r3(median(all.map(fn)));
  const p95 = fn => { const v = all.map(fn).sort((a, b) => a - b); return r3(v[Math.floor(v.length * 0.95)]); };
  const creatures = f => d(f, 0) + (prepass ? d(f, 2) : 0) + d(f, 3);
  const res = {
    shadow: m(f => d(f, 0)),
    terrain: m(f => d(f, 1)),
    prepass: prepass ? m(f => d(f, 2)) : 0,
    shade: m(f => d(f, 3)),
    creatures: m(creatures),
    creatures_p95: p95(creatures),
    frame_gpu: m(f => Number(ts[f * Q + 7] - ts[f * Q]) / 1e6),
    throughput_ms: r3(throughput),
    individuals: sc.inds.length,
    tris: sc.tris,
    creature_pixels: sc.pixels,
    creature_pixel_share: r3(sc.pixels / (W * H)),
  };
  res.ns_per_creature_pixel_shade = sc.pixels ? r3(res.shade * 1e6 / sc.pixels) : null;
  return res;
}

/** The kill criterion's other half: does the frame hold 60 fps when submitted at 60 Hz?
 *  GPU clocks are whatever the OS chooses at that load. Pacing is a busy-wait on a 16.67 ms
 *  deadline, because the browser pane is hidden while this runs, which pauses
 *  requestAnimationFrame and throttles timers. */
async function paced(sc, mode, prepass, frames = 180) {
  const Q = 8, period = 1000 / 60;
  const qs = device.createQuerySet({ type: 'timestamp', count: frames * Q });
  const t0 = performance.now() + 20;
  for (let f = 0; f < frames; f++) {
    const deadline = t0 + f * period;
    while (performance.now() < deadline) { /* spin */ }
    update(sc, f / 60);
    const enc = device.createCommandEncoder();
    encodeFrame(enc, sc, mode, prepass, qs, f * Q);
    device.queue.submit([enc.finish()]);
  }
  await device.queue.onSubmittedWorkDone();
  const ts = await resolveTimestamps(qs, frames * Q);
  const keep = [...Array(frames).keys()].slice(30);
  const gpu = keep.map(f => Number(ts[f * Q + 7] - ts[f * Q]) / 1e6);
  const spacing = keep.slice(1).map(f => Number(ts[f * Q] - ts[(f - 1) * Q]) / 1e6);
  const sorted = [...gpu].sort((a, b) => a - b);
  return {
    pacing: 'busy-wait at 60 Hz (pane hidden)',
    frames: keep.length,
    frame_gpu_median: r3(median(gpu)),
    frame_gpu_p95: r3(sorted[Math.floor(sorted.length * 0.95)]),
    frame_gpu_max: r3(sorted[sorted.length - 1]),
    frame_gpu_over_16_7: gpu.filter(g => g > 16.7).length,
    gpu_start_spacing_median: r3(median(spacing)),
    gpu_start_spacing_max: r3(Math.max(...spacing)),
  };
}

async function resolveTimestamps(qs, count) {
  const buf = buffer(count * 8, U.QUERY_RESOLVE | U.COPY_SRC);
  const enc = device.createCommandEncoder();
  enc.resolveQuerySet(qs, 0, count, buf, 0);
  device.queue.submit([enc.finish()]);
  const raw = await readback([{ src: buf, size: count * 8 }]);
  buf.destroy();
  qs.destroy();
  return new BigUint64Array(raw, 0, count);
}

/** Visible creature pixels: terrain and the creature prepass fill depth, then creatures are drawn
 *  again with depth 'equal' into a coverage target, and a compute pass counts covered texels. */
async function coverage(sc) {
  update(sc, 0);
  const enc = device.createCommandEncoder();
  encodeFrame(enc, sc, 'vertex', true, null, 0);
  let p = enc.beginRenderPass({
    colorAttachments: [{ view: T.coverageView, clearValue: [0, 0, 0, 0], loadOp: 'clear', storeOp: 'store' }],
    depthStencilAttachment: { view: T.depthView, depthLoadOp: 'load', depthStoreOp: 'store' },
  });
  p.setPipeline(P.coverage);
  p.setBindGroup(0, B.frame);
  for (const d of sc.draws) {
    p.setBindGroup(1, d.bg);
    p.setVertexBuffer(0, d.ind.verts);
    p.setIndexBuffer(d.ind.indices, 'uint32');
    p.drawIndexedIndirect(d.ind.draw, 0);
  }
  p.end();
  enc.clearBuffer(B.count);
  const c = enc.beginComputePass();
  c.setPipeline(P.count);
  c.setBindGroup(0, B.countBG);
  c.dispatchWorkgroups(Math.ceil(W / 16), Math.ceil(H / 16));
  c.end();
  device.queue.submit([enc.finish()]);
  return new Uint32Array(await readback([{ src: B.count, size: 16 }]))[0];
}

async function snapshot(name) {
  present();
  const blob = await new Promise(r => $('view').toBlob(r, 'image/png'));
  if (blob) await save(`${name}.png`, blob);
}

// ---- CPU (wasm) ----------------------------------------------------------------------------------

async function cpuBench() {
  const { instance } = await WebAssembly.instantiateStreaming(fetch('cpu.wasm'));
  const x = instance.exports;
  const g = makeGrazer(1);
  new Float32Array(x.memory.buffer, x.params_ptr(), PARAM_FLOATS).set(g.params);
  x.load();
  const out = () => new Float64Array(x.memory.buffer, x.out_ptr(), 16);
  const time = (fn, min = 400) => {
    fn();
    let n = 0, r;
    const t0 = performance.now();
    do { r = fn(); n++; } while (performance.now() - t0 < min);
    return { ms: (performance.now() - t0) / n, r };
  };
  const N = 200000;
  const full = time(() => x.bench_eval_js(N, 0));
  const pruned = time(() => x.bench_eval_js(N, 1));
  R.cpu.evals_per_ms = { full: Math.round(N / full.ms), pruned: Math.round(N / pruned.ms) };
  log('cpu evals/ms', JSON.stringify(R.cpu.evals_per_ms));
  x.lipschitz_probe_js(200000);
  let o = out();
  R.cpu.lipschitz = { max: r3(o[0]), mean: r3(o[1]), share_over_1_5: o[2] };
  R.cpu.mass = {};
  for (const finest of [0.02, 0.01, 0.005]) {
    const t = time(() => x.mass_js(finest), 200);
    o = out();
    R.cpu.mass[finest] = { ms: r3(t.ms), mass_kg: r3(o[0]), volume_m3: o[1], com: [o[2], o[3], o[4]].map(r3), evals: o[5], leaves: o[6], inside_nodes: o[7] };
  }
  log('cpu mass', JSON.stringify(R.cpu.mass));
  const rays = 20000;
  const rt = time(() => x.raycast_bench_js(rays));
  o = out();
  R.cpu.raycast = { us_per_ray: r3(rt.ms * 1000 / rays), avg_evals: r3(o[0]), hit_share: o[1] };
  R.cpu.raycast.ms_per_tick_160_rays = r3(R.cpu.raycast.us_per_ray * 160 / 1000);
  log('cpu raycast', JSON.stringify(R.cpu.raycast));
  x.det_hash_js();
  o = out();
  R.cpu.det_hash = o[0].toString(16).padStart(8, '0') + o[1].toString(16).padStart(8, '0');
  log('cpu det_hash', R.cpu.det_hash);
}

// ---- run -------------------------------------------------------------------------------------------

async function runAll() {
  $('run').disabled = true;
  try {
    await init();
    log('device', JSON.stringify(R.device));
    if (!$('skipcpu').checked) await cpuBench();

    // Pipelines: cold one at a time, cold all at once, then warm (same source as the first).
    const saltA = 1000 + Math.random() * 1000, saltB = 3000 + Math.random() * 1000;
    R.pipelines.cold_sequential = (await pipelines(saltA, false)).ms;
    R.pipelines.cold_parallel_total = (await pipelines(saltB, true)).ms.total;
    R.pipelines.warm_sequential = (await pipelines(saltA, false)).ms;
    const made = await pipelines(null, true);
    Object.assign(P, made.out);
    R.pipelines.per_scene = 'cull, place, emit, shadow, prepass, one shade, terrain (+ blit) = 8';
    log('pipelines', JSON.stringify(R.pipelines));
    await toolPipelines();

    // Extraction at several cell sizes. 1.5cm meets GRAZER_LOOK's 1mm tolerance on the 4.2cm
    // leg radius (chord error h²/8r); 3cm stands in for a lower LOD.
    const keep = {};
    for (const cell of [0.03, 0.02, 0.015, 0.01]) {
      const { res, inds } = await extractHerd(cell, HERD, cell === 0.015 || cell === 0.03);
      R.extraction[cell] = res;
      if (cell === 0.015 || cell === 0.03) keep[cell] = inds;
      log(`extract ${cell}`, JSON.stringify(res));
    }

    frameResources();
    const inst = herdInstances();
    const close = [{ x: 0, z: 0, y: terrainHeight(0, 0), yaw: Math.PI / 2, phase: 0.3, headDown: 0.2 }];
    const scenes = [
      scene('herd', keep[0.015], inst, [2, 7.5, -27], [0, 0.8, 1], 26),
      scene('herd_lod3cm', keep[0.03], inst, [2, 7.5, -27], [0, 0.8, 1], 26),
      scene('closeup', [keep[0.015][0]], close, [0.85, 1.4, -3.6], [0.85, 1.2, 0], 4),
    ];
    R.paced = {};
    for (const sc of scenes) {
      sc.pixels = await coverage(sc);
      for (const mode of ['field', 'lookup', 'vertex']) {
        for (const prepass of [true, false]) {
          const key = `${sc.name}/${mode}${prepass ? '' : '/no-prepass'}`;
          R.frames[key] = await measure(sc, mode, prepass);
          log(key, JSON.stringify(R.frames[key]));
          if (prepass) await snapshot(`${sc.name}-${mode}`);
        }
      }
      const pre = R.frames[`${sc.name}/field`].creatures <= R.frames[`${sc.name}/field/no-prepass`].creatures;
      const key = `${sc.name}/field${pre ? '' : '/no-prepass'}`;
      R.paced[key] = await paced(sc, 'field', pre);
      log('paced', key, JSON.stringify(R.paced[key]));
    }
    R.finished = new Date().toISOString();
    await save(`run-${R.started.replace(/[:.]/g, '-')}.json`, JSON.stringify(R, null, 2));
    log('done');
  } catch (e) {
    log('FAILED:', e.stack || e.message);
    R.error = String(e.stack || e.message);
  }
  await save('DONE', R.error ? 'failed' : 'ok');   // tells spikes/headless.sh the run is over
  $('run').disabled = false;
}

/** A fast smoke test: compile, extract a few grazers, draw one frame of each mode. */
async function quick() {
  await init();
  Object.assign(P, (await pipelines(null, true)).out);
  await toolPipelines();
  const { res, inds } = await extractHerd(0.015, HERD, true);
  log('extract', JSON.stringify(res));
  frameResources();
  const herd = scene('herd', inds, herdInstances(), [2, 7.5, -27], [0, 0.8, 1], 26);
  const close = scene('closeup', [inds[0]], [{ x: 0, z: 0, y: terrainHeight(0, 0), yaw: Math.PI / 2, phase: 0.3, headDown: 0.2 }], [0.85, 1.4, -3.6], [0.85, 1.2, 0], 4);
  window.__show = (which, mode = 'field', t = 0) => {
    const sc = which === 'close' ? close : herd;
    update(sc, t);
    const enc = device.createCommandEncoder();
    encodeFrame(enc, sc, mode, true, null, 0);
    device.queue.submit([enc.finish()]);
    present();
  };
  window.__snap = snapshot;
  window.__cam = (eye, target) => { close.viewproj = mat4.mul(mat4.perspective(40 * Math.PI / 180, W / H, 0.1, 400), mat4.lookAt(eye, target, [0, 1, 0])); close.eye = eye; };
  window.__show('herd');
  log('quick done');
}

$('run').addEventListener('click', runAll);
if (location.hash === '#run') runAll();
if (location.hash === '#quick') quick().catch(e => log('FAILED:', e.stack || e.message));
