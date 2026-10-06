// The decoder's mapping to WebGPU, on a fake device (see runtime/native/tests/suite/gpu.rs for
// the same behaviour on a real GPU).

import { describe, expect, test } from "bun:test";
import { buildPipelines, GpuExecutor, ShaderError } from "../src/gpu.ts";
import type { Manifest } from "../src/manifest.ts";
import { NONE } from "../src/abi.gen.ts";
import { decode } from "../src/stream.ts";
import { Encoder, words } from "./encoder.ts";
import { type Event, FakeDevice, fakeTexture } from "./fake-gpu.ts";
import { shapes } from "./fixtures.ts";
import type { ScreenTarget } from "../src/gpu.ts";

const SHADER = "// fine\n";

async function setup(manifest: Manifest = shapes(), screen: ScreenTarget = { texture: () => fakeTexture("screen") }) {
  const device = new FakeDevice();
  const pipelines = await buildPipelines(device.gpu, manifest, manifest.pipelines.map(() => SHADER));
  const executor = new GpuExecutor(device.gpu, pipelines, screen);
  /** Runs a batch's commands (already valid: these tests are about the mapping). */
  const run = (e: Encoder) => {
    for (const cmd of decode(e.finish())) executor.execute(cmd);
  };
  return { device, executor, run };
}

const kinds = (events: Event[]) => events.map((e) => e.kind);
const u = (n: number) => words([n, n, n, n]);

describe("pipelines", () => {
  test("lay out bind group 0 from the manifest", async () => {
    const { device } = await setup();
    const [render, compute] = device.layouts.map((l) => Array.from(l.entries));
    const { VERTEX, FRAGMENT, COMPUTE } = GPUShaderStage;
    expect(render).toEqual([
      { binding: 0, visibility: VERTEX | FRAGMENT, buffer: { type: "uniform", hasDynamicOffset: true, minBindingSize: 16 } },
      { binding: 1, visibility: VERTEX | FRAGMENT, buffer: { type: "read-only-storage" } },
      // Writable storage isn't allowed in vertex shaders.
      { binding: 2, visibility: FRAGMENT, buffer: { type: "storage" } },
    ]);
    expect(compute![2]).toEqual({ binding: 2, visibility: COMPUTE, buffer: { type: "storage" } });
  });

  test("report WGSL errors with the pipeline, shader and line", async () => {
    const device = new FakeDevice();
    const m = shapes();
    const build = buildPipelines(device.gpu, m, [SHADER, "fn main() {\n  ERROR;\n}"]);
    await expect(build).rejects.toThrow(ShaderError);
    await expect(buildPipelines(device.gpu, m, [SHADER, "\nERROR"])).rejects.toThrow(
      "pipeline `compute` (shader compute.wgsl) failed to build:\n2:3: unresolved identifier",
    );
  });

  test("report validation errors from building", async () => {
    const device = new FakeDevice();
    device.pipelineErrors.set("compute", "entry point `main` not found");
    await expect(buildPipelines(device.gpu, shapes(), [SHADER, SHADER])).rejects.toThrow(
      "pipeline `compute` (shader compute.wgsl) failed to build:\nentry point `main` not found",
    );
  });

  test("report each layout's validation error against its own pipeline, though they build at once", async () => {
    const device = new FakeDevice();
    device.layoutErrors.set("draw", "binding 1 is used twice");
    const build = buildPipelines(device.gpu, shapes(), [SHADER, SHADER]);
    await expect(build).rejects.toThrow("pipeline `draw` (shader draw.wgsl) failed to build:\nbinding 1 is used twice");
  });

  test("reject uniform blocks over the device's limit", async () => {
    const m = shapes();
    m.pipelines[0]!.uniform = { binding: 0, size: 65_552, space: "uniform" };
    await expect(buildPipelines(new FakeDevice().gpu, m, [SHADER, SHADER])).rejects.toThrow(
      "its uniform block is 65552 bytes; the limit is 65536",
    );
  });
});

test("buffers are created zeroed, as storage that can be copied both ways and hold indirect arguments", async () => {
  const { device, run } = await setup();
  run(new Encoder().createBuffer(3, 64));
  const b = device.buffers.find((b) => b.label === "buffer 3")!;
  expect(b.size).toBe(64);
  const { STORAGE, COPY_DST, COPY_SRC, INDIRECT, INDEX } = GPUBufferUsage;
  expect(b.usage).toBe(STORAGE | COPY_DST | COPY_SRC | INDIRECT | INDEX);
});

test("a write is a copy from the upload ring in order with the work around it, not a submission", async () => {
  const { device, run, executor } = await setup();
  run(
    new Encoder()
      .createBuffer(1, 64)
      .createBuffer(2, 64)
      .writeBuffer(1, 0, words([7]))
      .dispatch(1, [2, 1, 1], [1, 2], u(5))
      .writeBuffer(1, 4, words([8])),
  );
  executor.flush();
  expect(kinds(device.events)).toEqual(["copyBuffer", "dispatch", "copyBuffer", "writeBuffer", "writeBuffer", "submit"]);
  // Each write's bytes go into the upload ring, written once just before the submission, with the
  // dispatch's uniforms into theirs.
  const [first, , second, ring, upload] = device.events as Extract<Event, { kind: "copyBuffer" | "writeBuffer" }>[];
  expect(first).toMatchObject({ kind: "copyBuffer", from: "upload ring", fromOffset: 0, to: "buffer 1", toOffset: 0, size: 4 });
  expect(second).toMatchObject({ kind: "copyBuffer", from: "upload ring", fromOffset: 4, to: "buffer 1", toOffset: 4, size: 4 });
  expect(ring).toMatchObject({ kind: "writeBuffer", buffer: "uniform ring" });
  expect(Array.from((ring as Extract<Event, { kind: "writeBuffer" }>).data)).toEqual(Array.from(u(5)));
  expect(upload).toMatchObject({ kind: "writeBuffer", buffer: "upload ring" });
  expect(Array.from((upload as Extract<Event, { kind: "writeBuffer" }>).data)).toEqual([7, 0, 0, 0, 8, 0, 0, 0]);
});

test("each dispatch and draw gets its own aligned uniform slice", async () => {
  const { device, run } = await setup();
  const e = new Encoder().createBuffer(1, 64).createBuffer(2, 64);
  for (let i = 0; i < 3; i++) e.dispatch(1, [1, 1, 1], [1, 2], u(i));
  e.beginScreenPass([0.25, 0.5, 0.75, 1]);
  for (let i = 3; i < 5; i++) e.draw(0, 3, 1, [1, 2], u(i));
  run(e.present());
  // The dispatches' submission happens at Present, with the pass, in one submit.
  expect(kinds(device.events)).toEqual(["dispatch", "dispatch", "dispatch", "renderPass", "draw", "draw", "writeBuffer", "submit"]);
  const offsets = device.events.flatMap((ev) => ("offsets" in ev ? ev.offsets : []));
  expect(offsets).toEqual([0, 256, 512, 768, 1024]);
  const ring = (device.events[6] as Extract<Event, { kind: "writeBuffer" }>).data;
  offsets.forEach((offset, i) => expect(Array.from(ring.subarray(offset, offset + 16))).toEqual(Array.from(u(i))));
  expect(device.events[3]).toEqual({ kind: "renderPass", clear: { r: 0.25, g: 0.5, b: 0.75, a: 1 }, view: "screen" });
});

test("bind groups are cached per pipeline and buffers", async () => {
  const { device, run } = await setup();
  run(
    new Encoder()
      .createBuffer(1, 64)
      .createBuffer(2, 64)
      .dispatch(1, [1, 1, 1], [1, 2], u(0))
      .dispatch(1, [1, 1, 1], [1, 2], u(1))
      .dispatch(1, [1, 1, 1], [2, 1], u(2)),
  );
  const ids = device.events.flatMap((e) => (e.kind === "dispatch" ? [e.bindGroup.id] : []));
  expect(ids).toEqual([0, 0, 1]);
  const bg = (device.events[0] as Extract<Event, { kind: "dispatch" }>).bindGroup;
  expect(bg.entries).toEqual([
    { binding: 0, buffer: "uniform ring", offset: 0, size: 16 },
    { binding: 1, buffer: "buffer 1", offset: 0, size: 64 },
    { binding: 2, buffer: "buffer 2", offset: 0, size: 64 },
  ]);
});

test("a debug build's flag is laid out, bound in every dispatch and draw, and read", async () => {
  const m = shapes();
  for (const p of m.pipelines) p.debug_flag = 3;
  const { device, executor, run } = await setup(m);
  const { FRAGMENT, COMPUTE } = GPUShaderStage;
  const [render, compute] = device.layouts.map((l) => Array.from(l.entries));
  // Not in vertex shaders, which can't write storage.
  expect(render).toContainEqual({ binding: 3, visibility: FRAGMENT, buffer: { type: "storage" } });
  expect(compute).toContainEqual({ binding: 3, visibility: COMPUTE, buffer: { type: "storage" } });
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)));
  const bg = (device.events.find((e) => e.kind === "dispatch") as Extract<Event, { kind: "dispatch" }>).bindGroup;
  expect(bg.entries).toContainEqual({ binding: 3, buffer: "debug flag", offset: undefined, size: undefined });
  // The fake GPU's flag stays 0: no pipeline went out of range.
  await executor.checkDebugFlag();
});

test("destroying a buffer drops just the bind groups that use it", async () => {
  const { device, run } = await setup();
  const e = new Encoder().createBuffer(1, 64).createBuffer(2, 64).createBuffer(3, 64);
  e.dispatch(1, [1, 1, 1], [1, 2], u(0)).dispatch(1, [1, 1, 1], [2, 3], u(1));
  e.destroyBuffer(1).createBuffer(1, 64);
  run(e.dispatch(1, [1, 1, 1], [1, 2], u(2)).dispatch(1, [1, 1, 1], [2, 3], u(3)));
  const groups = device.events.flatMap((ev) => (ev.kind === "dispatch" ? [ev.bindGroup] : []));
  expect(groups.map((g) => g.id)).toEqual([0, 1, 2, 1]);
  const [first, second] = device.buffers.filter((b) => b.label === "buffer 1");
  expect(groups[0]!.buffers[1]).toBe(first!);
  expect(groups[2]!.buffers[1]).toBe(second!);
});

test("a destroyed buffer is released once the work recorded before it is submitted", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)).destroyBuffer(1));
  const buffer = device.buffers.find((b) => b.label === "buffer 1")!;
  expect(buffer.destroyed).toBe(false);
  executor.flush();
  expect(buffer.destroyed).toBe(true);
});

test("a destroyed buffer is released in a frame with no GPU work", async () => {
  const { device, run, executor } = await setup();
  for (let frame = 0; frame < 3; frame++) {
    run(new Encoder().createBuffer(1, 1 << 20).writeBuffer(1, 0, words([7])).destroyBuffer(1));
    executor.flush(); // the end of the frame
  }
  const buffers = device.buffers.filter((b) => b.label === "buffer 1");
  expect(buffers.map((b) => b.destroyed)).toEqual([true, true, true]);
});

test("the ring grows when one pass needs more than it holds", async () => {
  const { device, run } = await setup();
  const e = new Encoder().createBuffer(1, 64).createBuffer(2, 64).beginScreenPass([0, 0, 0, 1]);
  const n = 300; // 300 x 256 bytes > the 64 KiB ring
  for (let i = 0; i < n; i++) e.draw(0, 3, 1, [1, 2], u(i));
  run(e.present());
  const rings = device.buffers.filter((b) => b.label === "uniform ring");
  expect(rings.map((r) => [r.size, r.destroyed])).toEqual([
    [65_536, true],
    [131_072, false],
  ]);
  const draws = device.events.filter((ev) => ev.kind === "draw");
  expect(draws.length).toBe(n);
  expect(draws.every((d) => d.bindGroup.entries[0]!.size === 16)).toBe(true);
  const write = device.events.find((ev) => ev.kind === "writeBuffer") as Extract<Event, { kind: "writeBuffer" }>;
  expect(write.data.length).toBe((n - 1) * 256 + 16);
  expect(Array.from(write.data.subarray(299 * 256, 299 * 256 + 4))).toEqual([43, 1, 0, 0]);
});

test("growing the ring drops the bind groups that point at the old one", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)));
  executor.flush();
  const e = new Encoder().beginScreenPass([0, 0, 0, 1]);
  for (let i = 0; i < 300; i++) e.draw(0, 3, 1, [1, 2], u(i)); // 300 x 256 bytes > the 64 KiB ring
  run(e.present().dispatch(1, [1, 1, 1], [1, 2], u(1)));
  const [small, large] = device.buffers.filter((b) => b.label === "uniform ring");
  const groups = device.events.flatMap((ev) => (ev.kind === "dispatch" || ev.kind === "draw" ? [ev.bindGroup] : []));
  expect(groups[0]!.buffers[0]).toBe(small!);
  // The last dispatch's pipeline and buffers are the first's, but its group is new.
  expect(groups.slice(1).every((g) => g.buffers[0] === large)).toBe(true);
  expect(groups.at(-1)!.id).not.toBe(groups[0]!.id);
});

test("dispatches flush when the ring is full, then reuse it", async () => {
  const { device, run } = await setup();
  const e = new Encoder().createBuffer(1, 64).createBuffer(2, 64);
  for (let i = 0; i < 257; i++) e.dispatch(1, [1, 1, 1], [1, 2], u(i));
  run(e);
  // 256 slices fill the ring; the 257th submits them and starts again at 0.
  const offsets = device.events.flatMap((ev) => (ev.kind === "dispatch" ? ev.offsets : []));
  expect(offsets[255]).toBe(255 * 256);
  expect(offsets[256]).toBe(0);
  expect(kinds(device.events).filter((k) => k === "submit").length).toBe(1);
  expect(device.buffers.filter((b) => b.label === "uniform ring").length).toBe(1);
});

test("a screen target can copy each pass out (test mode)", async () => {
  const screen = fakeTexture("readable");
  const { device, run } = await setup(shapes(), {
    texture: () => screen,
    afterPass: (encoder) => encoder.copyTextureToTexture({ texture: screen }, { texture: fakeTexture("canvas") }, [1, 1]),
  });
  run(new Encoder().beginScreenPass([0, 0, 0, 1]).present());
  expect(device.events).toEqual([
    { kind: "renderPass", clear: { r: 0, g: 0, b: 0, a: 1 }, view: "readable" },
    { kind: "copy", from: "readable", to: "canvas" },
    { kind: "submit" },
  ]);
});

/** `shapes()` with the render pipeline binding a texture and a sampler instead of buffers. */
function sampled(): Manifest {
  const m = shapes();
  m.pipelines[0]!.bindings = [
    { binding: 1, kind: "texture" },
    { binding: 2, kind: "comparison_sampler" },
  ];
  return m;
}

const offscreen = (color: number, depth: number, keep = false) => ({
  color,
  keepColor: keep,
  clear: [0, 0, 0, 1] as [number, number, number, number],
  depth,
  keepDepth: false,
  clearDepth: 1,
});

test("textures and samplers are made as the stream says, and bound as views and samplers", async () => {
  const { device, run } = await setup(sampled());
  run(
    new Encoder()
      .createTexture(3, 4, 2, "rgba16float")
      .createTexture(4, 4, 2, "depth32float")
      .createSampler(5, true, true, "less-equal")
      .writeTexture(3, [1, 0], [2, 2], new Uint8Array(32))
      .beginScreenPass([0, 0, 0, 1])
      .draw(0, 3, 1, [[4, 0, 0], [5, 0, 0]], u(0))
      .present(),
  );
  const [color, depth] = device.textures;
  const { TEXTURE_BINDING, RENDER_ATTACHMENT, COPY_SRC, COPY_DST } = GPUTextureUsage;
  expect([color!.format, color!.size, color!.usage]).toEqual(["rgba16float", [4, 2], TEXTURE_BINDING | RENDER_ATTACHMENT | COPY_SRC | COPY_DST]);
  // A depth texture can't be written from the CPU.
  expect([depth!.format, depth!.usage]).toEqual(["depth32float", TEXTURE_BINDING | RENDER_ATTACHMENT | COPY_SRC]);
  expect(device.samplers[0]).toMatchObject({ magFilter: "linear", addressModeU: "repeat", compare: "less-equal" });
  expect(device.events.find((e) => e.kind === "writeTexture")).toEqual({
    kind: "writeTexture",
    texture: "texture 3",
    origin: { x: 1, y: 0 },
    bytesPerRow: 16,
    size: { width: 2, height: 2 },
  });
  const draw = device.events.find((e) => e.kind === "draw") as Extract<Event, { kind: "draw" }>;
  expect(draw.bindGroup.entries.map((e) => e.buffer)).toEqual(["uniform ring", "texture 4", "sampler 5"]);
});

test("a pass into textures draws with the pipeline made for their formats, and isn't submitted alone", async () => {
  const { device, run, executor } = await setup();
  run(
    new Encoder()
      .createBuffer(1, 64)
      .createBuffer(2, 64)
      .createTexture(3, 8, 8, "rgba8unorm")
      .createTexture(4, 8, 8, "depth32float")
      .createTexture(5, 8, 8, "rgba16float")
      .beginPass(offscreen(3, 4))
      .draw(0, 3, 1, [1, 2], u(0))
      .endPass()
      .beginPass(offscreen(NONE, 4))
      .draw(0, 3, 1, [1, 2], u(1))
      .endPass()
      .beginPass(offscreen(3, NONE, true))
      .draw(0, 3, 1, [1, 2], u(2))
      .endPass()
      .beginPass(offscreen(5, NONE))
      .draw(0, 3, 1, [1, 2], u(3))
      .endPass(),
  );
  // The screen's format with and without a depth target, and a depth pass, were made at load;
  // another format's variant is made at its first draw.
  expect(device.variants).toEqual(["draw rgba16float|none"]);
  expect(device.events.filter((e) => e.kind === "renderPass").slice(0, 3)).toEqual([
    { kind: "renderPass", clear: { r: 0, g: 0, b: 0, a: 1 }, view: "texture 3", depth: "texture 4 clear" },
    { kind: "renderPass", clear: { r: 0, g: 0, b: 0, a: 0 }, view: "none", depth: "texture 4 clear" },
    { kind: "renderPass", clear: { r: 0, g: 0, b: 0, a: 1 }, view: "texture 3", load: "load" },
  ]);
  expect(kinds(device.events)).not.toContain("submit");
  executor.flush();
  expect(kinds(device.events).at(-1)).toBe("submit");
});

test("copies and indirect work are recorded in order", async () => {
  const { device, run } = await setup();
  run(
    new Encoder()
      .createBuffer(1, 512)
      .createBuffer(2, 512)
      .createBuffer(3, 64)
      .copyBuffer(1, 4, 2, 8, 12)
      .dispatchIndirect(1, 3, 0, [[1, 0, 256], [2, 256, 256]], u(0))
      .beginScreenPass([0, 0, 0, 1])
      .drawIndirect(0, 3, 16, [[1, 256, 64], [2, 0, 64]], u(1))
      .present(),
  );
  expect(device.events.filter((e) => e.kind !== "writeBuffer" && e.kind !== "submit").map((e) => ({ ...e, ...("bindGroup" in e ? { bindGroup: null } : {}) }))).toEqual([
    { kind: "copyBuffer", from: "buffer 1", fromOffset: 4, to: "buffer 2", toOffset: 8, size: 12 },
    { kind: "dispatchIndirect", pipeline: "compute", buffer: "buffer 3", offset: 0 },
    { kind: "renderPass", clear: { r: 0, g: 0, b: 0, a: 1 }, view: "screen" },
    { kind: "drawIndirect", pipeline: "draw", buffer: "buffer 3", offset: 16 },
  ]);
});

test("a span binds its range of the buffer", async () => {
  const { device, run } = await setup();
  run(new Encoder().createBuffer(1, 1024).createBuffer(2, 1024).dispatch(1, [1, 1, 1], [[1, 256, 512], [2, 0, 128]], u(0)));
  const d = device.events.find((e) => e.kind === "dispatch") as Extract<Event, { kind: "dispatch" }>;
  expect(d.bindGroup.entries.slice(1)).toEqual([
    { binding: 1, buffer: "buffer 1", offset: 256, size: 512 },
    { binding: 2, buffer: "buffer 2", offset: 0, size: 128 },
  ]);
});

test("a readback submits the work before it, then copies its range out", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)));
  const staging = executor.readBack(2, 8, 12);
  expect(kinds(device.events)).toEqual(["dispatch", "writeBuffer", "submit", "copyBuffer", "submit"]);
  const readback = device.buffers.find((b) => b.label === "readback")!;
  expect([readback.size, readback.usage]).toEqual([12, GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST]);
  readback.contents = Uint8Array.of(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);
  expect(Array.from(await staging)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
  expect(readback.destroyed).toBe(true);
});

test("destroying a texture releases it after the work before it is submitted", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createTexture(3, 8, 8, "rgba8unorm").destroyTexture(3).createSampler(4, false, false, null).destroySampler(4));
  expect(device.textures[0]!.destroyed).toBe(false);
  executor.flush();
  expect(device.textures[0]!.destroyed).toBe(true);
});
