// Spike 01's numbers for the Rust harness (compiler/tests/src/spike01.rs), from its own
// grazer.js, unchanged: each seed's parameters and bounds; the herd's placement and each
// scene's `View` uniform (main.js's `herdInstances` and `scene`, copied here: main.js runs its
// benchmark on load and exports nothing); and bone palettes at given times (grazer.js's `pose`,
// as main.js's `update` calls it).
//
//   bun data.mjs seeds                       JSON: seeds 1 to 40, the placement, the views
//   bun data.mjs palettes <scene> <t>...     binary: for each t, each instance's 29 mat4s (f32)
//
// <scene> is `herd` (seeds 1 to 40 as placed) or `closeup` (seed 1 at the origin).

import { makeGrazer, pose, terrainHeight, rng, mat4, BONES } from '../fixtures/spike01/grazer.js';

const W = 1920, H = 1080, HERD = 40;

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

/** A scene's `View` uniform at t = 0 (main.js's `scene` and `update`): view-projection, the
 *  sun's view-projection, the eye and t, and the direction toward the sun. */
function view(eye, target, shadowExtent) {
  const sun = (() => { const v = [-0.5, 0.75, -0.45], l = Math.hypot(...v); return v.map(x => x / l); })();
  const center = [target[0], 0, target[2]];
  const lightEye = center.map((c, i) => c + sun[i] * 60);
  const light = mat4.mul(mat4.ortho(-shadowExtent, shadowExtent, -shadowExtent, shadowExtent, 1, 140), mat4.lookAt(lightEye, center, [0, 1, 0]));
  const viewproj = mat4.mul(mat4.perspective(40 * Math.PI / 180, W / H, 0.1, 400), mat4.lookAt(eye, target, [0, 1, 0]));
  const v = new Float32Array(40);
  v.set(viewproj, 0);
  v.set(light, 16);
  v.set([...eye, 0], 32);
  v.set([...sun, 0], 36);
  return Array.from(v);
}

const close = [{ x: 0, z: 0, y: terrainHeight(0, 0), yaw: Math.PI / 2, phase: 0.3, headDown: 0.2 }];
const grazers = Array.from({ length: HERD }, (_, i) => makeGrazer(i + 1));
const [what, ...args] = process.argv.slice(2);

if (what === 'seeds') {
  const seeds = grazers.map(g => ({ seed: g.seed, params: Array.from(g.params), min: g.bounds.min, max: g.bounds.max }));
  const views = { herd: view([2, 7.5, -27], [0, 0.8, 1], 26), closeup: view([0.85, 1.4, -3.6], [0.85, 1.2, 0], 4) };
  process.stdout.write(JSON.stringify({ seeds, herd: herdInstances(), closeup: close, views }));
} else if (what === 'palettes') {
  const [scene, ...times] = args;
  const inst = scene === 'closeup' ? close : herdInstances();
  const gs = scene === 'closeup' ? [grazers[0]] : grazers;
  const out = new Float32Array(times.length * gs.length * BONES * 16);
  times.forEach((t, k) => gs.forEach((g, i) => pose(g, inst[i], Number(t), out, (k * gs.length + i) * BONES * 16)));
  process.stdout.write(new Uint8Array(out.buffer));
} else {
  console.error('usage: bun data.mjs seeds | palettes <herd|closeup> <t>...');
  process.exit(2);
}
