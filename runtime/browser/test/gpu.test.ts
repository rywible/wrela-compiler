// The decoder's mapping to WebGPU, on a fake device (see runtime/native/tests/suite/gpu.rs for
// the same behaviour on a real GPU).

import { describe, expect, spyOn, test } from "bun:test";
import { type BuiltPipeline, buildPipelines, preparePipelines, GpuExecutor, type PipelineCache, screenFormat, ShaderError, shaderSource } from "../src/gpu.ts";
import type { Manifest, Pipeline } from "../src/manifest.ts";
import { NONE, SCREEN_FORMAT, TEXTURE_FORMATS, UPLOAD_MAX, UPLOAD_START } from "../src/abi.gen.ts";
import { decode, onePass, type Pass } from "../src/stream.ts";
import { Encoder, words } from "./encoder.ts";
import { buildAll, type Event, FakeDevice, fakeTexture, SCREEN_TARGET, SHADER } from "./fake-gpu.ts";
import { shapes } from "./fixtures.ts";
import type { ScreenTarget } from "../src/gpu.ts";

async function setup(manifest: Manifest = shapes(), screen: ScreenTarget = SCREEN_TARGET) {
  const device = new FakeDevice();
  const pipelines = await Promise.all(buildPipelines(device.gpu, manifest, manifest.pipelines.map(() => SHADER), screen.format).each);
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
    const build = Promise.all(buildPipelines(device.gpu, m, [SHADER, "fn main() {\n  ERROR;\n}"], SCREEN_FORMAT).each);
    await expect(build).rejects.toThrow(ShaderError);
    await expect(Promise.all(buildPipelines(device.gpu, m, [SHADER, "\nERROR"], SCREEN_FORMAT).each)).rejects.toThrow(
      "pipeline `compute` (shader compute.wgsl) failed to build:\n2:3: unresolved identifier",
    );
  });

  test("report validation errors from building", async () => {
    const device = new FakeDevice();
    device.pipelineErrors.set("compute", "entry point `main` not found");
    await expect(Promise.all(buildPipelines(device.gpu, shapes(), [SHADER, SHADER], SCREEN_FORMAT).each)).rejects.toThrow(
      "pipeline `compute` (shader compute.wgsl) failed to build:\nentry point `main` not found",
    );
  });

  test("report each layout's validation error against its own pipeline, though they build at once", async () => {
    const device = new FakeDevice();
    device.layoutErrors.set("draw", "binding 1 is used twice");
    const build = Promise.all(buildPipelines(device.gpu, shapes(), [SHADER, SHADER], SCREEN_FORMAT).each);
    await expect(build).rejects.toThrow("pipeline `draw` (shader draw.wgsl) failed to build:\nbinding 1 is used twice");
  });

  test("reject uniform blocks over the device's limit", async () => {
    const m = shapes();
    m.pipelines[0]!.uniform = { binding: 0, size: 65_552, space: "uniform" };
    await expect(Promise.all(buildPipelines(new FakeDevice().gpu, m, [SHADER, SHADER], SCREEN_FORMAT).each)).rejects.toThrow(
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

test("small texture writes keep their order in one submission, with aligned copies of packed rows", async () => {
  for (const format of TEXTURE_FORMATS.filter((f) => !f.depth && f.block === 1)) {
    const { device, run, executor } = await setup();
    const bytes = Uint8Array.from({ length: 3 * 2 * format.bytes }, (_, i) => i + 1);
    const original = bytes.slice();
    run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).createTexture(3, 5, 4, format.name));
    run(new Encoder().dispatch(1, [1, 1, 1], [1, 2], u(0)).writeBuffer(1, 0, words([7])));
    executor.execute({ op: "WriteTexture", handle: 3, level: 0, layer: 0, x: 1, y: 1, width: 3, height: 2, data: bytes });
    bytes.fill(255); // The program may reuse its shared memory before the submission.
    run(new Encoder().dispatch(1, [1, 1, 1], [1, 2], u(1)));
    executor.execute({ op: "WriteTexture", handle: 3, level: 0, layer: 0, x: 0, y: 0, width: 0, height: 4, data: new Uint8Array(0) });
    executor.flush();
    expect(kinds(device.events)).toEqual([
      "dispatch", "copyBuffer", "copyBufferToTexture", "dispatch", "writeBuffer", "writeBuffer", "submit",
    ]);
    expect(device.events[2]).toEqual({
      kind: "copyBufferToTexture", from: "upload ring", fromOffset: 256, texture: "texture 3",
      origin: { x: 1, y: 1, z: 0 }, bytesPerRow: 256, rowsPerImage: 2, size: { width: 3, height: 2 },
    });
    const upload = device.events[5] as Extract<Event, { kind: "writeBuffer" }>;
    expect(upload.data.length).toBe(768);
    expect(upload.data.subarray(0, 4)).toEqual(words([7]));
    expect(upload.data.subarray(256, 256 + 3 * format.bytes)).toEqual(original.subarray(0, 3 * format.bytes));
    expect(upload.data.subarray(512, 512 + 3 * format.bytes)).toEqual(original.subarray(3 * format.bytes));
  }
});

test("texture writes flush and reuse a full upload ring", async () => {
  const { device, run, executor } = await setup();
  const data = new Uint8Array(UPLOAD_START / 2);
  run(new Encoder().createTexture(3, 128, 256, "rgba8unorm"));
  for (let i = 0; i < 3; i++) {
    executor.execute({ op: "WriteTexture", handle: 3, level: 0, layer: 0, x: 0, y: 0, width: 128, height: 256, data });
  }
  executor.flush();
  expect(kinds(device.events)).toEqual([
    "copyBufferToTexture", "copyBufferToTexture", "writeBuffer", "submit", "copyBufferToTexture", "writeBuffer", "submit",
  ]);
  const offsets = device.events.flatMap((e) => e.kind === "copyBufferToTexture" ? [e.fromOffset] : []);
  expect(offsets).toEqual([0, data.length, 0]);
  expect(device.buffers.filter((b) => b.label === "upload ring").length).toBe(1);
});

test("a texture write grows the upload ring after earlier work is submitted", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createBuffer(1, 64).writeBuffer(1, 0, words([7])).createTexture(3, 64, 1025, "rgba8unorm"));
  const data = new Uint8Array(256 * 1025);
  executor.execute({ op: "WriteTexture", handle: 3, level: 0, layer: 0, x: 0, y: 0, width: 64, height: 1025, data });
  executor.flush();
  expect(kinds(device.events)).toEqual(["copyBuffer", "writeBuffer", "submit", "copyBufferToTexture", "writeBuffer", "submit"]);
  expect(device.buffers.filter((b) => b.label === "upload ring").map((b) => [b.size, b.destroyed])).toEqual([
    [UPLOAD_START, true], [2 * UPLOAD_START, false],
  ]);
});

test("a texture with a staged write is destroyed only after its copy is submitted", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createTexture(3, 1, 1, "rgba8unorm").writeTexture(3, [0, 0], [1, 1], words([7])).destroyTexture(3));
  expect(device.textures[0]!.destroyed).toBe(false);
  executor.flush();
  expect(kinds(device.events)).toEqual(["copyBufferToTexture", "writeBuffer", "submit"]);
  expect(device.textures[0]!.destroyed).toBe(true);
});

test("texture uploads over the padded size cap still write through the queue after a flush", async () => {
  const { device, run, executor } = await setup();
  // The packed bytes fit the cap, but the padded rows don't.
  const width = 2049;
  const height = Math.floor(UPLOAD_MAX / 4352) + 1;
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).createTexture(3, width, height, "r16float"));
  run(new Encoder().dispatch(1, [1, 1, 1], [1, 2], u(0)));
  executor.execute({ op: "WriteTexture", handle: 3, level: 0, layer: 0, x: 0, y: 0, width, height, data: new Uint8Array(2 * width * height) });
  expect(kinds(device.events)).toEqual(["dispatch", "writeBuffer", "submit", "writeTexture"]);
  expect(device.events[3]).toMatchObject({ bytesPerRow: 2 * width, size: { width, height } });
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

test("each pipeline's unchanged ranges reuse its group without another string-key lookup", async () => {
  const m = shapes();
  m.pipelines.push({ ...m.pipelines[1]!, name: "another compute" });
  const { device, run } = await setup(m);
  const get = spyOn(Map.prototype, "get");
  let lookups = 0;
  try {
    const e = new Encoder().createBuffer(1, 64).createBuffer(2, 64);
    for (let i = 0; i < 3; i++) e.dispatch(1, [1, 1, 1], [1, 2], u(i)).dispatch(2, [1, 1, 1], [1, 2], u(i));
    run(e);
    lookups = get.mock.calls.filter(([key]) => typeof key === "string" && /^\d+:\d+@/.test(key)).length;
  } finally {
    get.mockRestore();
  }
  expect(lookups).toBe(2);
  expect(device.events.flatMap((e) => (e.kind === "dispatch" ? [e.bindGroup.id] : []))).toEqual([0, 1, 0, 1, 0, 1]);
});

test("a cached group's ranges stay its own when a command changes its bindings", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64));
  const cmd = decode(new Encoder().dispatch(1, [1, 1, 1], [1, 2], u(0)).finish())[0]!;
  if (cmd.op !== "Dispatch") throw new Error("expected a dispatch");
  executor.execute(cmd);
  cmd.bindings[0]!.offset = 4;
  cmd.bindings[0]!.size = 60;
  executor.execute(cmd);
  cmd.bindings[0]!.size = 32;
  executor.execute(cmd);
  cmd.bindings[0]!.handle = 2;
  executor.execute(cmd);
  cmd.bindings[0] = { handle: 1, offset: 0, size: 64 };
  executor.execute(cmd);
  executor.execute(cmd);
  const groups = device.events.flatMap((e) => (e.kind === "dispatch" ? [e.bindGroup] : []));
  expect(groups.map((g) => g.id)).toEqual([0, 1, 2, 3, 0, 0]);
  expect(groups[0]!.entries[1]).toMatchObject({ buffer: "buffer 1", offset: 0, size: 64 });
  expect(groups[3]!.entries[1]).toMatchObject({ buffer: "buffer 2", offset: 4, size: 32 });
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
    format: SCREEN_FORMAT,
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
    { binding: 1, kind: "texture", stage: "both", format: "rgba8unorm" },
    { binding: 2, kind: "comparison_sampler", stage: "both", format: null },
  ];
  return m;
}

/** A pass into texture `color` (none if NONE), its colour cleared unless `keep`, and `depth`
 * (or NONE), cleared. */
const offscreen = (color: number, depth: number, keep = false): Pass => {
  const pass = onePass(color, keep, [0, 0, 0, 1]);
  return { ...pass, colors: color === NONE ? [] : pass.colors, depth };
};

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
  expect([color!.format, color!.size, color!.usage]).toEqual(["rgba16float", [4, 2, 1], TEXTURE_BINDING | RENDER_ATTACHMENT | COPY_SRC | COPY_DST]);
  // A depth texture can't be written from the CPU.
  expect([depth!.format, depth!.usage]).toEqual(["depth32float", TEXTURE_BINDING | RENDER_ATTACHMENT | COPY_SRC]);
  expect(device.samplers[0]).toMatchObject({ magFilter: "linear", addressModeU: "repeat", compare: "less-equal" });
  expect(device.events.find((e) => e.kind === "copyBufferToTexture")).toEqual({
    kind: "copyBufferToTexture",
    from: "upload ring",
    fromOffset: 0,
    texture: "texture 3",
    origin: { x: 1, y: 0, z: 0 },
    bytesPerRow: 256,
    rowsPerImage: 2,
    size: { width: 2, height: 2 },
  });
  const draw = device.events.find((e) => e.kind === "draw") as Extract<Event, { kind: "draw" }>;
  expect(draw.bindGroup.entries.map((e) => e.buffer)).toEqual(["uniform ring", "texture 4", "sampler 5"]);
});

test("replacing a texture or sampler invalidates the pipeline's last bind group", async () => {
  const { device, run } = await setup(sampled());
  const draw = (e: Encoder) => e.beginScreenPass([0, 0, 0, 1]).draw(0, 3, 1, [[4, 0, 0], [5, 0, 0]], u(0)).present();
  run(draw(new Encoder().createTexture(4, 4, 2, "depth32float").createSampler(5, true, true, "less-equal")));
  run(draw(new Encoder().destroyTexture(4).createTexture(4, 4, 2, "depth32float")));
  run(draw(new Encoder().destroySampler(5).createSampler(5, false, false, "less-equal")));
  run(draw(new Encoder()));
  const groups = device.events.flatMap((e) => (e.kind === "draw" ? [e.bindGroup.id] : []));
  expect(groups).toEqual([0, 1, 2, 2]);
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
  expect(kinds(device.events)).not.toContain("submit");
  // The last pass waits for what follows (it may be joined) and is recorded at the flush.
  executor.flush();
  expect(kinds(device.events).at(-1)).toBe("submit");
  // Each of the pipeline's targets' variants was made at load: none at a draw.
  expect(device.variants).toEqual([]);
  expect(device.made).toContain("draw rgba16float|none");
  expect(device.made).toContain("draw none|depth32float");
  expect(device.events.filter((e) => e.kind === "renderPass").slice(0, 3)).toEqual([
    { kind: "renderPass", clear: { r: 0, g: 0, b: 0, a: 1 }, view: "texture 3", depth: "texture 4 clear" },
    { kind: "renderPass", clear: { r: 0, g: 0, b: 0, a: 0 }, view: "none", depth: "texture 4 clear" },
    { kind: "renderPass", clear: { r: 0, g: 0, b: 0, a: 1 }, view: "texture 3", load: "load" },
  ]);
});

test("a render pipeline's variants are its manifest's targets, made at load", async () => {
  const device = new FakeDevice();
  const m = shapes();
  const draw = m.pipelines[0]!;
  if (draw.kind !== "render") throw new Error("the shapes' pipeline 0 renders");
  draw.targets = [
    { colors: ["rgba16float"], depth: true, screen: false },
    { colors: [], depth: true, screen: false },
  ];
  await buildAll(device, m);
  expect(device.made).toEqual(["draw rgba16float|depth32float", "draw none|depth32float"]);
});

describe("the screen's format", () => {
  /** The shapes, with the drawing pipeline's targets these. */
  const drawnInto = (targets: Extract<Pipeline, { kind: "render" }>["targets"]) => {
    const m = shapes();
    const draw = m.pipelines[0]!;
    if (draw.kind !== "render") throw new Error("the shapes' pipeline 0 renders");
    draw.targets = targets;
    return m;
  };
  const targets = drawnInto([
    { colors: ["rgba8unorm"], depth: false, screen: true },
    { colors: ["rgba8unorm"], depth: false, screen: false },
    { colors: ["rgba16float"], depth: true, screen: false },
  ]);

  test("is the canvas's preferred format in a normal run, and SCREEN_FORMAT in test mode", () => {
    const original = Object.getOwnPropertyDescriptor(navigator, "gpu");
    Object.defineProperty(navigator, "gpu", { configurable: true, value: { getPreferredCanvasFormat: () => "bgra8unorm" } });
    try {
      expect(screenFormat(false)).toBe("bgra8unorm");
      expect(screenFormat(true)).toBe(SCREEN_FORMAT);
    } finally {
      if (original) Object.defineProperty(navigator, "gpu", original);
      else delete (navigator as unknown as Record<string, unknown>).gpu;
    }
  });

  test("is what screen targets are built in; a texture target keeps its own", async () => {
    const device = new FakeDevice();
    await Promise.all(buildPipelines(device.gpu, targets, [SHADER, SHADER], "bgra8unorm").each);
    expect(device.made).toEqual(["draw bgra8unorm|none", "draw rgba8unorm|none", "draw rgba16float|depth32float"]);
  });

  test("shares a variant with a texture target of the same format", async () => {
    const device = new FakeDevice();
    await Promise.all(buildPipelines(device.gpu, targets, [SHADER, SHADER], SCREEN_FORMAT).each);
    expect(device.made).toEqual(["draw rgba8unorm|none", "draw rgba16float|depth32float"]);
  });

  test("is what a screen pass draws in", async () => {
    const m = drawnInto([{ colors: ["rgba8unorm"], depth: false, screen: true }]);
    const { device, run, executor } = await setup(m, { format: "bgra8unorm", texture: () => fakeTexture("canvas") });
    expect(device.made).toEqual(["draw bgra8unorm|none"]);
    run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).beginScreenPass([0, 0, 0, 1]).draw(0, 3, 1, [1, 2], u(0)).present());
    executor.flush();
    expect(device.events.filter((e) => e.kind === "draw").map((e) => e.kind === "draw" && e.pipeline)).toEqual(["draw"]);
  });
});

test("a pass that may join the one before, on the same targets kept, runs as part of it", async () => {
  const { device, run, executor } = await setup();
  run(
    new Encoder()
      .createBuffer(1, 64)
      .createBuffer(2, 64)
      .createTexture(3, 8, 8, "rgba8unorm")
      .createTexture(4, 8, 8, "depth32float")
      .label("joins")
      .beginPass(offscreen(3, 4))
      .draw(0, 3, 1, [1, 2], u(0))
      .endPass()
      .beginPass({ ...offscreen(3, 4, true), keepDepth: true, join: true })
      .draw(0, 3, 1, [1, 2], u(1))
      .endPass()
      .beginPass({ ...offscreen(3, 4, true), keepDepth: true })
      .draw(0, 3, 1, [1, 2], u(2))
      .endPass(),
  );
  executor.flush();
  // The second runs in the first's render pass; the third, which may not join, is its own.
  expect(device.events.filter((e) => e.kind === "renderPass").length).toBe(2);
  expect(device.events.filter((e) => e.kind === "draw").length).toBe(3);
});

test("a pass joins only one of its own label: a label between them records the one before", async () => {
  const device = new FakeDevice();
  device.features.add("timestamp-query");
  const executor = new GpuExecutor(device.gpu, await buildAll(device, shapes()), SCREEN_TARGET);
  const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
  const joining = { ...offscreen(3, NONE, true), join: true };
  run(new Encoder().timed(1, "B"));
  const asked = executor.timeLabel("B").then(() => "answered");
  run(
    new Encoder()
      .createBuffer(1, 64)
      .createBuffer(2, 64)
      .createTexture(3, 8, 8, "rgba8unorm")
      .label("A")
      .beginPass(offscreen(3, NONE))
      .draw(0, 3, 1, [1, 2], u(0))
      .endPass()
      .label("B")
      .beginPass(joining)
      .draw(0, 3, 1, [1, 2], u(1))
      .endPass()
      // The same name given again still records the pass before; the pass after it joins.
      .label("B")
      .beginPass(joining)
      .draw(0, 3, 1, [1, 2], u(2))
      .endPass()
      .beginPass(joining)
      .draw(0, 3, 1, [1, 2], u(3))
      .endPass(),
  );
  executor.flush();
  expect(device.passLabels).toEqual(["A", "B", "B"]);
  expect(device.events.filter((e) => e.kind === "draw")).toHaveLength(4);
  // B's work is B's, so its ask is answered.
  executor.frame = 1;
  expect(await Promise.race([asked, Bun.sleep(100).then(() => "pending")])).toBe("answered");
  executor.destroyAll();
});

test("passes with conflicting buffer access stay separate even when their targets may join", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).createTexture(3, 8, 8, "rgba8unorm")
    .beginPass(offscreen(3, NONE)).draw(0, 3, 1, [1, 2], u(0)).endPass()
    .beginPass({ ...offscreen(3, NONE, true), join: true }).draw(0, 3, 1, [2, 1], u(1)).endPass());
  executor.flush();
  expect(device.events.filter((e) => e.kind === "renderPass")).toHaveLength(2);
  expect(device.events.filter((e) => e.kind === "draw")).toHaveLength(2);
});

test("pass joining includes indirect argument and index reads in its buffer access checks", async () => {
  for (const indexed of [false, true]) {
    const { device, run, executor } = await setup();
    const commands = new Encoder().createBuffer(1, 64).createBuffer(2, 64).createBuffer(3, 64).createBuffer(4, 64)
      .createTexture(5, 8, 8, "rgba8unorm").beginPass(offscreen(5, NONE));
    if (indexed) commands.drawIndexedIndirect(0, [1, 0, 64], 4, 0, [2, 3], u(0));
    else commands.drawIndirect(0, 1, 0, [2, 3], u(0));
    run(commands.endPass().beginPass({ ...offscreen(5, NONE, true), join: true })
      .draw(0, 3, 1, [2, 1], u(1)).endPass());
    executor.flush();
    expect(device.events.filter((e) => e.kind === "renderPass")).toHaveLength(2);
    const indirect = device.events.find((e) => e.kind === (indexed ? "drawIndexedIndirect" : "drawIndirect"));
    expect(indirect).toBeDefined();
  }
});

test("a label names the passes and dispatches after it, until the next label or frame", async () => {
  const { device, run, executor } = await setup();
  run(
    new Encoder()
      .createBuffer(1, 64)
      .createBuffer(2, 64)
      .createTexture(3, 8, 8, "rgba8unorm")
      .dispatch(1, [1, 1, 1], [[1, 0, 64], [2, 0, 64]], u(0))
      .label("look")
      .dispatch(1, [1, 1, 1], [[1, 0, 64], [2, 0, 64]], u(0))
      .beginPass(offscreen(3, NONE))
      .draw(0, 3, 1, [1, 2], u(1))
      .endPass()
      .label("glow")
      .beginPass(offscreen(3, NONE))
      .draw(0, 3, 1, [1, 2], u(2))
      .endPass(),
  );
  executor.frame = 1;
  run(new Encoder().beginPass(offscreen(3, NONE)).draw(0, 3, 1, [1, 2], u(3)).endPass());
  executor.flush();
  expect(device.passLabels).toEqual(["compute", "look", "look", "glow", "pass"]);
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

test("a failed readback releases its staging buffer", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createBuffer(1, 64));
  const createBuffer = device.createBuffer.bind(device);
  device.createBuffer = (desc) => {
    const buffer = createBuffer(desc);
    if (desc.label === "readback") buffer.mapAsync = async () => { throw new Error("map failed"); };
    return buffer;
  };
  await expect(executor.readBack(1, 0, 4)).rejects.toThrow("map failed");
  expect(device.buffers.find((b) => b.label === "readback")?.destroyed).toBe(true);
  const before = device.buffers.length;
  expect(() => executor.readBack(99, 0, 4)).toThrow("buffer 99 passed the checks but doesn't exist");
  expect(device.buffers).toHaveLength(before);
});

test("a command whose pipeline is still being built waits for it, and the commands after it wait behind it", async () => {
  const device = new FakeDevice();
  const m = shapes();
  const [draw, compute] = await buildAll(device, m);
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [draw!, later.promise], SCREEN_TARGET);
  const e = new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)).writeBuffer(1, 0, words([7]));
  for (const cmd of decode(e.finish())) executor.execute(cmd);
  const answer = executor.readBack(2, 0, 4);
  executor.flush();
  // The buffers are made; the dispatch, the write after it and the readback wait.
  expect(executor.waiting).toBe(true);
  expect(kinds(device.events)).toEqual([]);
  const caughtUp = executor.caughtUp();
  later.resolve(compute!);
  await caughtUp;
  expect(executor.waiting).toBe(false);
  // The readback after them copies once they're submitted.
  expect(kinds(device.events)).toEqual(["dispatch", "copyBuffer", "writeBuffer", "writeBuffer", "submit", "copyBuffer", "submit"]);
  expect((await answer).length).toBe(4);
});

test.each(["off", "serial"] as const)("unchecked, a dispatch or draw of a pipeline the manifest doesn't have fails at once (timing %s)", async (timing) => {
  const device = new FakeDevice();
  const executor = new GpuExecutor(device.gpu, await buildAll(device, shapes()), SCREEN_TARGET, timing);
  const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64));
  expect(() => run(new Encoder().dispatch(2, [1, 1, 1], [1, 2], u(0)))).toThrow("Dispatch failed: there's no pipeline 2 (the manifest has 2)");
  expect(() => run(new Encoder().beginScreenPass([0, 0, 0, 1]).draw(7, 3, 1, [1, 2], u(0)))).toThrow("Draw failed: there's no pipeline 7 (the manifest has 2)");
  // Neither waits for a pipeline that will never be built.
  expect(executor.waiting).toBe(false);
  await executor.drain();
  executor.destroyAll();
});

test("a command backlog advances across staggered pipelines and keeps writes and readbacks in order", async () => {
  const device = new FakeDevice();
  const m = shapes();
  m.pipelines.push({ ...m.pipelines[1]!, name: "another compute" });
  const [draw, compute, another] = await buildAll(device, m);
  const first = Promise.withResolvers<BuiltPipeline>();
  const second = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [draw!, first.promise, second.promise], SCREEN_TARGET);
  const run = (e: Encoder) => {
    for (const cmd of decode(e.finish())) executor.execute(cmd);
  };
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)).writeBuffer(1, 0, words([7])));
  const read = executor.readBack(2, 0, 4);
  run(new Encoder().dispatch(2, [2, 1, 1], [1, 2], u(1)).writeBuffer(1, 4, words([8])));
  const caughtUp = executor.caughtUp();
  first.resolve(compute!);
  await first.promise;
  expect(executor.waiting).toBe(true);
  expect(kinds(device.events)).toEqual(["dispatch", "copyBuffer", "writeBuffer", "writeBuffer", "submit", "copyBuffer", "submit"]);
  run(new Encoder().dispatch(1, [3, 1, 1], [1, 2], u(2)));
  second.resolve(another!);
  await caughtUp;
  expect(executor.waiting).toBe(false);
  expect((await read).length).toBe(4);
  expect(device.events.flatMap((e) => (e.kind === "dispatch" ? [[e.pipeline, e.groups[0]]] : []))).toEqual([
    ["compute", 1], ["another compute", 2], ["compute", 3],
  ]);
  // Commands after the backlog drains execute immediately.
  run(new Encoder().dispatch(1, [4, 1, 1], [1, 2], u(3)));
  expect(device.events.at(-1)).toMatchObject({ kind: "dispatch", groups: [4, 1, 1] });
});

test.each(["waiting", "serial"] as const)("a %s draw keeps one uniform copy until its pass ends", async (mode) => {
  const device = new FakeDevice();
  const m = shapes();
  const [draw, compute] = await buildAll(device, m);
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [mode === "waiting" ? later.promise : draw!, compute!], SCREEN_TARGET, mode === "serial" ? "serial" : "off");
  const batch = new Encoder().createBuffer(1, 64).createBuffer(2, 64).beginScreenPass([0, 0, 0, 1]).draw(0, 3, 1, [1, 2], u(7)).finish();
  const commands = decode(batch);
  const slice = spyOn(Uint8Array.prototype, "slice");
  let copies = 0;
  try {
    for (const cmd of commands) executor.execute(cmd);
    batch.fill(0);
    if (mode === "waiting") {
      const caughtUp = executor.caughtUp();
      later.resolve(draw!);
      await caughtUp;
    } else await executor.drain();
    copies = slice.mock.calls.length;
  } finally {
    slice.mockRestore();
  }
  expect(copies).toBe(1);
  executor.execute({ op: "Present" });
  await executor.drain();
  const uploaded = device.events.find((e) => e.kind === "writeBuffer") as Extract<Event, { kind: "writeBuffer" }>;
  expect(uploaded.data).toEqual(u(7));
});

test("a reentrant immediate draw owns its uniforms while a queued screen pass finishes", async () => {
  const device = new FakeDevice();
  const m = shapes();
  const [draw, compute] = await buildAll(device, m);
  const later = Promise.withResolvers<BuiltPipeline>();
  const reentrant = decode(new Encoder().beginScreenPass([0, 0, 0, 1]).draw(0, 3, 1, [1, 2], u(9)).finish());
  const reentrantDraw = reentrant[1]!;
  if (reentrantDraw.op !== "Draw") throw new Error("expected a draw");
  let reentered = false;
  const executor = new GpuExecutor(device.gpu, [later.promise, compute!], {
    format: SCREEN_FORMAT,
    texture: () => fakeTexture("screen"),
    afterPass: () => {
      if (reentered) return;
      reentered = true;
      // Present is the final waiting command, so these commands run immediately while the
      // queued command still supplies its frame's timing context. Leave this pass open.
      for (const cmd of reentrant) executor.execute(cmd);
    },
  });
  for (const cmd of decode(new Encoder().createBuffer(1, 64).createBuffer(2, 64).beginScreenPass([0, 0, 0, 1]).draw(0, 3, 1, [1, 2], u(7)).present().finish())) executor.execute(cmd);
  const caughtUp = executor.caughtUp();
  later.resolve(draw!);
  await caughtUp;
  expect(reentered).toBe(true);
  expect(executor.waiting).toBe(false);
  expect(device.events.filter((e) => e.kind === "draw")).toHaveLength(1);
  reentrantDraw.uniforms.fill(0);
  executor.execute({ op: "Present" });
  const uploaded = device.events.filter((e): e is Extract<Event, { kind: "writeBuffer" }> => e.kind === "writeBuffer" && e.buffer === "uniform ring");
  expect(uploaded.map((e) => e.data)).toEqual([u(7), u(9)]);
});

test("commands that wait for a pipeline keep their frame's label; the next frame's start with none", async () => {
  const device = new FakeDevice();
  const m = shapes();
  const [draw, compute] = await buildAll(device, m);
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [draw!, later.promise], SCREEN_TARGET);
  const run = (e: Encoder) => {
    for (const cmd of decode(e.finish())) executor.execute(cmd);
  };
  const dispatch = (e: Encoder) => e.dispatch(1, [1, 1, 1], [1, 2], u(0));
  run(dispatch(dispatch(new Encoder().createBuffer(1, 64).createBuffer(2, 64).label("look"))));
  // The next frame begins while the last one's dispatches still wait.
  executor.frame = 1;
  run(dispatch(new Encoder()));
  const caughtUp = executor.caughtUp();
  later.resolve(compute!);
  await caughtUp;
  // The last frame's two dispatches, both labelled, share a compute pass; the next frame's has
  // its pipeline's name.
  expect(device.passLabels).toEqual(["look", "compute"]);
});

test("texture passes waiting for a pipeline keep separate timings in their recorded frames", async () => {
  for (const join of [false, true]) {
    const device = new FakeDevice();
    device.features.add("timestamp-query");
    const m = shapes();
    const [draw, compute] = await buildAll(device, m);
    const later = Promise.withResolvers<BuiltPipeline>();
    const executor = new GpuExecutor(device.gpu, [later.promise, compute!], SCREEN_TARGET, "span");
    const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
    run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).createTexture(3, 8, 8, "rgba8unorm")
      .label("first").beginPass(offscreen(3, NONE)).draw(0, 3, 1, [1, 2], u(0)).endPass());
    executor.frame = 1;
    run(new Encoder().label("second").beginPass({ ...offscreen(3, NONE, true), join })
      .draw(0, 3, 1, [1, 2], u(1)).endPass());
    const caughtUp = executor.caughtUp();
    later.resolve(draw!);
    await caughtUp;
    expect((await executor.timings()).map(({ frame, label }) => ({ frame, label }))).toEqual([
      { frame: 0, label: "first" }, { frame: 1, label: "second" },
    ]);
    executor.destroyAll();
  }
});

test("a command that waited for a pipeline, then fails, fails the program", async () => {
  const device = new FakeDevice();
  const m = shapes();
  const [draw, compute] = await buildAll(device, m);
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [draw!, later.promise], SCREEN_TARGET);
  // Buffers that don't exist: what no checked command binds, so it fails when it runs.
  for (const cmd of decode(new Encoder().dispatch(1, [1, 1, 1], [8, 9], u(0)).finish())) executor.execute(cmd);
  const caughtUp = executor.caughtUp().then(
    () => "caught up",
    (e: Error) => e.message,
  );
  later.resolve(compute!);
  expect(await caughtUp).toBe("host bug: resource 8 passed the checks but doesn't exist");
  expect(() => executor.waiting).toThrow("host bug: resource 8 passed the checks but doesn't exist");
});

test("a failed waiting queue does not submit recorded work when another pipeline finishes", async () => {
  const device = new FakeDevice();
  const m = shapes();
  const [draw, compute] = await buildAll(device, m);
  const first = Promise.withResolvers<BuiltPipeline>();
  const unused = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [draw!, first.promise, unused.promise], SCREEN_TARGET);
  const batch = new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)).destroyBuffer(8);
  for (const cmd of decode(batch.finish())) executor.execute(cmd);
  const caughtUp = executor.caughtUp();
  first.resolve(compute!);
  await expect(caughtUp).rejects.toThrow("host bug: resource 8 passed the checks but doesn't exist");
  expect(kinds(device.events)).toEqual(["dispatch"]);
  unused.resolve(compute!);
  await executor.built;
  expect(kinds(device.events)).toEqual(["dispatch"]);
});

test("a hot reload keeps the pipelines whose entries and WGSL are the same, wherever their files are", async () => {
  const device = new FakeDevice();
  const cache: PipelineCache = new Map();
  const numbered = (ps: Pipeline[]) => ps.map((p, i) => ({ ...p, shader: `pipeline_${i}.wgsl` }));
  const m = shapes();
  m.pipelines = numbered(m.pipelines);
  const first = buildPipelines(device.gpu, m, [SHADER, SHADER], SCREEN_FORMAT, cache).each;
  // The next build has a pipeline before them: each of their files is one further on.
  const next = shapes();
  next.pipelines = numbered([{ ...next.pipelines[1]!, name: "another" }, ...next.pipelines]);
  const second = buildPipelines(device.gpu, next, [SHADER, SHADER, SHADER], SCREEN_FORMAT, cache).each;
  expect(second[1]).toBe(first[0]!);
  expect(second[2]).toBe(first[1]!);
  expect(second[0]).not.toBe(first[1]!);
  await Promise.all(second);
});

test("a pipeline that can't be built fails the program: caughtUp and built reject", async () => {
  const device = new FakeDevice();
  const m = shapes();
  const [draw] = await buildAll(device, m);
  const failing = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [draw!, failing.promise], SCREEN_TARGET);
  for (const cmd of decode(new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)).finish())) {
    executor.execute(cmd);
  }
  const why = (p: Promise<void>) => p.then(() => "built", (e: Error) => e.message);
  const caughtUp = why(executor.caughtUp());
  const built = why(executor.built);
  failing.reject(new Error("pipeline `compute` failed to build"));
  expect(await caughtUp).toBe("pipeline `compute` failed to build");
  expect(await built).toBe("pipeline `compute` failed to build");
});

test("a late pipeline success runs no commands after another pipeline failed", async () => {
  const device = new FakeDevice();
  const failed = Promise.withResolvers<BuiltPipeline>();
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [failed.promise, later.promise], SCREEN_TARGET);
  const compute: BuiltPipeline = {
    name: "compute",
    layout: {} as GPUBindGroupLayout,
    compute: { label: "compute" } as GPUComputePipeline,
    render: null,
    uniform: null,
    bindings: [],
    debugFlag: null,
  };
  executor.execute({ op: "Dispatch", pipeline: 1, groups: [1, 1, 1], bindings: [], uniforms: new Uint8Array() });
  const readback = executor.readBack(7, 0, 4);
  failed.reject(new Error("pipeline 0 failed"));
  await executor.built.catch(() => {});
  await expect(readback).rejects.toThrow("pipeline 0 failed");
  later.resolve(compute);
  await later.promise;
  await Promise.resolve();
  expect(kinds(device.events)).toEqual([]);
  await expect(executor.caughtUp()).rejects.toThrow("pipeline 0 failed");
});

test("a retired executor runs no commands when a pipeline finishes late", async () => {
  const device = new FakeDevice();
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [later.promise], SCREEN_TARGET);
  const compute: BuiltPipeline = {
    name: "compute",
    layout: {} as GPUBindGroupLayout,
    compute: { label: "compute" } as GPUComputePipeline,
    render: null,
    uniform: null,
    bindings: [],
    debugFlag: null,
  };
  executor.execute({ op: "Dispatch", pipeline: 0, groups: [1, 1, 1], bindings: [], uniforms: new Uint8Array() });
  const readback = executor.readBack(7, 0, 4);
  executor.destroyAll();
  await expect(readback).rejects.toThrow("the GPU executor was retired");
  later.resolve(compute);
  await executor.built;
  await Promise.resolve();
  expect(kinds(device.events)).toEqual([]);
});

test("retiring an executor records an ended texture pass before releasing its resources", async () => {
  const { device, executor, run } = await setup();
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).createTexture(3, 8, 8, "rgba8unorm")
    .beginPass(offscreen(3, NONE)).draw(0, 3, 1, [1, 2], u(0)).endPass());
  executor.destroyAll();
  expect(device.events.filter((e) => e.kind === "draw")).toHaveLength(1);
  expect(device.events.at(-1)?.kind).toBe("submit");
  expect(device.textures.every((t) => t.destroyed)).toBe(true);
  expect(executor.memory.bytes).toBe(0);
});

test("retiring an executor releases its upload uniform and debug buffers", () => {
  const device = new FakeDevice();
  const executor = new GpuExecutor(device.gpu, [], SCREEN_TARGET, "off", true);
  expect(device.buffers.map((b) => b.label).sort()).toEqual(["debug flag", "readback", "uniform ring", "upload ring"]);
  executor.destroyAll();
  expect(device.buffers.every((b) => b.destroyed)).toBe(true);
  executor.destroyAll();
});

test("retiring an executor waits for a debug read before releasing its mapped buffer", async () => {
  const device = new FakeDevice();
  const executor = new GpuExecutor(device.gpu, [], SCREEN_TARGET, "off", true);
  const readback = device.buffers.find((buffer) => buffer.label === "readback")!;
  const mapped = Promise.withResolvers<void>();
  readback.mapAsync = () => mapped.promise;
  const checked = executor.checkDebugFlag();
  executor.destroyAll();
  expect(readback.destroyed).toBe(false);
  mapped.resolve();
  await checked;
  expect(device.buffers.every((buffer) => buffer.destroyed)).toBe(true);
});

test("a failed timestamp read releases its staging buffer and retirement releases its queries", async () => {
  const device = new FakeDevice();
  device.features.add("timestamp-query");
  const executor = new GpuExecutor(device.gpu, await buildAll(device, shapes()), SCREEN_TARGET, "span");
  const create = device.createBuffer.bind(device);
  device.createBuffer = (descriptor) => {
    const buffer = create(descriptor);
    if ((descriptor.usage & GPUBufferUsage.MAP_READ) !== 0) {
      buffer.mapAsync = async () => { throw new Error("timestamps couldn't map"); };
    }
    return buffer;
  };
  for (const cmd of decode(new Encoder().createBuffer(1, 64).createBuffer(2, 64)
    .dispatch(1, [1, 1, 1], [1, 2], u(0)).finish())) executor.execute(cmd);
  executor.flush();
  // The failed readback is done before timings() asks: its failure is kept all the same.
  await new Promise((resolve) => setTimeout(resolve, 0));
  await expect(executor.timings()).rejects.toThrow("timestamps couldn't map");
  expect(device.buffers.filter((buffer) => (buffer.usage & GPUBufferUsage.MAP_READ) !== 0)
    .every((buffer) => buffer.destroyed)).toBe(true);
  executor.destroyAll();
  expect(device.buffers.every((buffer) => buffer.destroyed)).toBe(true);
  expect(device.querySets.every((queries) => queries.destroyed)).toBe(true);
});

test("a pipeline failure rejects readbacks already taken by a serial drain", async () => {
  const device = new FakeDevice();
  const [draw] = await buildAll(device, shapes());
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [draw!, later.promise], SCREEN_TARGET, "serial");
  void executor.built.catch(() => {});
  for (const cmd of decode(new Encoder().createBuffer(1, 64).createBuffer(2, 64)
    .dispatch(1, [1, 1, 1], [1, 2], u(0)).finish())) executor.execute(cmd);
  const rejected = executor.readBack(2, 0, 4).then(() => "readback succeeded", (error: Error) => error.message);
  const drained = executor.drain();
  later.reject(new Error("serial pipeline failed"));
  await expect(drained).rejects.toThrow("serial pipeline failed");
  expect(await Promise.race([rejected, Promise.resolve("readback still pending")])).toBe("serial pipeline failed");
  executor.destroyAll();
});

test("retirement cancels readbacks and commands already taken by a serial drain", async () => {
  const device = new FakeDevice();
  const pipelines = await buildAll(device, shapes());
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [pipelines[0]!, later.promise], SCREEN_TARGET, "serial");
  for (const cmd of decode(new Encoder().createBuffer(1, 64).createBuffer(2, 64)
    .dispatch(1, [1, 1, 1], [1, 2], u(0)).finish())) executor.execute(cmd);
  const answer = executor.readBack(2, 0, 4).catch((error: Error) => error.message);
  const drained = executor.drain();
  executor.destroyAll();
  expect(await answer).toBe("the GPU executor was retired");
  later.resolve(pipelines[1]!);
  await drained;
  expect(device.events.filter((event) => event.kind === "dispatch")).toHaveLength(0);
});

test.each(["failed", "retired"] as const)("a partly drained backlog rejects readbacks when its executor is %s", async (mode) => {
  const device = new FakeDevice();
  const m = shapes();
  const [, compute] = await buildAll(device, m);
  const first = Promise.withResolvers<BuiltPipeline>();
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [first.promise, later.promise], SCREEN_TARGET);
  for (const cmd of decode(new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(0, [1, 1, 1], [1, 2], u(0)).dispatch(1, [2, 1, 1], [1, 2], u(1)).finish())) {
    executor.execute(cmd);
  }
  const caughtUp = executor.caughtUp().catch((e: Error) => e.message);
  const readback = executor.readBack(2, 0, 4).catch((e: Error) => e.message);
  const built = executor.built.catch((e: Error) => e.message);
  first.resolve(compute!);
  await first.promise;
  // The first dispatch's consumed slot is empty while the next pipeline is still missing.
  expect(executor.waiting).toBe(true);
  expect(device.events.filter((e) => e.kind === "dispatch")).toHaveLength(1);
  const why = mode === "failed" ? "later pipeline failed" : "the GPU executor was retired";
  if (mode === "failed") later.reject(new Error(why));
  else {
    executor.destroyAll();
    later.resolve(compute!);
  }
  expect(await caughtUp).toBe(why);
  expect(await readback).toBe(why);
  await built;
  expect(device.events.filter((e) => e.kind === "dispatch")).toHaveLength(1);
  if (mode === "failed") expect(() => executor.waiting).toThrow(why);
  else {
    expect(executor.waiting).toBe(false);
    await expect(executor.caughtUp()).resolves.toBeUndefined();
    await expect(executor.readBack(2, 0, 4)).rejects.toThrow(why);
  }
});

test("the program's buffers and textures are counted in bytes, now and at most", async () => {
  const { run, executor } = await setup();
  run(new Encoder().createBuffer(1, 1000).createTexture(2, 8, 4, "rgba16float").createTexture(3, 8, 8, "depth32float"));
  expect(executor.memory).toEqual({ bytes: 1000 + 256 + 256, peak: 1512 });
  run(new Encoder().destroyBuffer(1).destroyTexture(2));
  expect(executor.memory).toEqual({ bytes: 256, peak: 1512 });
});

test("destroying a texture releases it after the work before it is submitted", async () => {
  const { device, run, executor } = await setup();
  run(new Encoder().createTexture(3, 8, 8, "rgba8unorm").destroyTexture(3).createSampler(4, false, false, null).destroySampler(4));
  expect(device.textures[0]!.destroyed).toBe(false);
  executor.flush();
  expect(device.textures[0]!.destroyed).toBe(true);
});

test("background preparations share a bounded queue and a failed shader cannot poison it", async () => {
  const device = new FakeDevice();
  const m = shapes();
  m.pipelines = [0, 1, 2].map(i => ({ ...m.pipelines[1]!, name: `background ${i}`, shader: `${i}.wgsl` }));
  const started = Promise.withResolvers<void>();
  const finish = Promise.withResolvers<void>();
  const create = device.createComputePipelineAsync.bind(device);
  const seen: string[] = [];
  const spy = spyOn(device, "createComputePipelineAsync").mockImplementation(async descriptor => {
    seen.push(descriptor.label!);
    if (descriptor.label === "background 0") {
      started.resolve();
      await finish.promise;
      throw new Error("failed background shader");
    }
    return create(descriptor);
  });
  try {
    const first = preparePipelines(device.gpu, { ...m, pipelines: m.pipelines.slice(0, 2) }, [SHADER, SHADER], SCREEN_FORMAT, []);
    const second = preparePipelines(device.gpu, { ...m, pipelines: m.pipelines.slice(1) }, [SHADER, SHADER], SCREEN_FORMAT, [first]);
    const result = Promise.allSettled([...first.values(), ...second.values()]);
    expect([...first.values()][1]).toBe([...second.values()][0]);
    await started.promise;
    expect(seen).toEqual(["background 0"]);
    finish.resolve();
    const outcomes = await result;
    expect(outcomes.map(o => o.status)).toEqual(["rejected", "fulfilled", "fulfilled", "fulfilled"]);
    expect(seen).toEqual(["background 0", "background 1", "background 2"]);
    const failure = outcomes[0];
    expect(failure?.status === "rejected" && failure.reason.message).toContain("failed background shader");
    // A later call after the queue drains also works, with all held entries reused.
    const third = preparePipelines(device.gpu, { ...m, pipelines: m.pipelines.slice(1) }, [SHADER, SHADER], SCREEN_FORMAT, [second]);
    await Promise.all(third.values());
    expect(seen).toHaveLength(3);
  } finally {
    finish.resolve();
    spy.mockRestore();
  }
});

test("a program's ask for a label's time is answered once its frame has ended and all its timestamps are read", async () => {
  const device = new FakeDevice();
  device.features.add("timestamp-query");
  const m = shapes();
  const [, compute] = await buildAll(device, m);
  const executor = new GpuExecutor(device.gpu, [compute!, compute!], SCREEN_TARGET);
  expect(executor.timed).toBe(false);
  let answered: Uint8Array | null = null;
  const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
  // As a program asks: its `Timed` command runs, then its request starts.
  run(new Encoder().timed(1, "look"));
  const asked = executor.timeLabel("look").then((b) => (answered = b));
  expect(executor.timed).toBe(true);
  const dispatch = (e: Encoder) => e.dispatch(1, [1, 1, 1], [1, 2], u(0));
  // Frame 0's labelled work spans two submissions.
  run(dispatch(new Encoder().createBuffer(1, 64).createBuffer(2, 64).label("look")));
  executor.flush();
  run(dispatch(new Encoder().label("look")));
  executor.flush();
  await Promise.resolve();
  await Promise.resolve();
  expect(answered).toBeNull();
  executor.frame = 1;
  await asked;
  expect(answered!.length).toBe(4);
  expect(new Float32Array(answered!.buffer)[0]).toBe(0);
  executor.destroyAll();
});

test("an ask times only its own label's work: the rest runs untimed, its dispatches sharing a pass", async () => {
  const device = new FakeDevice();
  device.features.add("timestamp-query");
  const m = shapes();
  const [, compute] = await buildAll(device, m);
  const executor = new GpuExecutor(device.gpu, [compute!, compute!], SCREEN_TARGET);
  const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
  const twice = (e: Encoder) => e.dispatch(1, [1, 1, 1], [1, 2], u(0)).dispatch(1, [1, 1, 1], [1, 2], u(1));
  run(new Encoder().timed(1, "look"));
  const asked = executor.timeLabel("look");
  run(twice(new Encoder().createBuffer(1, 64).createBuffer(2, 64).label("other")));
  executor.flush();
  // Untimed, the two dispatches share one compute pass.
  expect(executor.takeCounters().compute_passes).toBe(1);
  run(twice(new Encoder().label("look")));
  executor.flush();
  // Timed, each is a pass of its own, for its timestamps.
  expect(executor.takeCounters().compute_passes).toBe(2);
  executor.frame = 1;
  await asked;
  // Answered, the label's work runs untimed again.
  run(twice(new Encoder().label("look")));
  executor.flush();
  expect(executor.takeCounters().compute_passes).toBe(1);
  executor.destroyAll();
});

test("an ask made while commands wait for a pipeline times only the work recorded after it", async () => {
  const device = new FakeDevice();
  device.features.add("timestamp-query");
  const [draw, compute] = await buildAll(device, shapes());
  const later = Promise.withResolvers<BuiltPipeline>();
  const executor = new GpuExecutor(device.gpu, [draw!, later.promise], SCREEN_TARGET);
  const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
  // Two dispatches of the label, then the ask, then one more: all wait for the pipeline.
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).label("look").dispatch(1, [1, 1, 1], [1, 2], u(0)).dispatch(1, [1, 1, 1], [1, 2], u(1)).timed(1, "look"));
  const asked = executor.timeLabel("look");
  run(new Encoder().dispatch(1, [1, 1, 1], [1, 2], u(2)));
  expect(executor.waiting).toBe(true);
  const caughtUp = executor.caughtUp();
  later.resolve(compute!);
  await caughtUp;
  executor.flush();
  // The two before the ask share an untimed pass; the one after it is timed, a pass of its own.
  expect(executor.takeCounters().compute_passes).toBe(2);
  executor.frame = 1;
  await asked;
  executor.destroyAll();
});

test("an ask times only its label's work recorded after it, also in the frame it was asked in", async () => {
  const device = new FakeDevice();
  device.features.add("timestamp-query");
  const create = device.createBuffer.bind(device);
  device.createBuffer = (descriptor) => {
    const buffer = create(descriptor);
    // The two dispatches' timestamps: X1 takes 2 µs, X2 7 µs.
    if (descriptor.label === "timestamps" && (descriptor.usage & GPUBufferUsage.MAP_READ) !== 0) {
      buffer.contents = new Uint8Array(new BigUint64Array([1000n, 3000n, 5000n, 12000n]).buffer);
    }
    return buffer;
  };
  const executor = new GpuExecutor(device.gpu, await buildAll(device, shapes()), SCREEN_TARGET);
  const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
  const micros = (bytes: Uint8Array) => new Float32Array(bytes.buffer)[0];
  // As a program asks: each `Timed` command runs, then its request starts.
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).label("look").timed(1, "look"));
  const first = executor.timeLabel("look").then(micros);
  run(new Encoder().dispatch(1, [1, 1, 1], [1, 2], u(0)).timed(2, "look"));
  const second = executor.timeLabel("look").then(micros);
  run(new Encoder().dispatch(1, [1, 1, 1], [1, 2], u(1)));
  executor.flush();
  executor.frame = 1;
  // The first ask gets X1 + X2; the second, made after X1, gets X2 alone.
  expect(await first).toBe(9);
  expect(await second).toBe(7);
  executor.destroyAll();
});

test("a GPU without timestamp queries refuses an ask for a time", async () => {
  const device = new FakeDevice();
  const m = shapes();
  const [, compute] = await buildAll(device, m);
  const executor = new GpuExecutor(device.gpu, [compute!, compute!], SCREEN_TARGET);
  // Each ask uses up its `Timed` command, refused or not: what waits for an ask is kept from
  // that command to its ask, and doesn't pile up frame after frame.
  const kept: string[] = [];
  const [set, remove] = [Map.prototype.set, Map.prototype.delete];
  const spies = [
    spyOn(Map.prototype, "set").mockImplementation(function (this: Map<unknown, unknown>, key, value) {
      if (key === "look") kept.push(`set ${JSON.stringify(value)}`);
      return set.call(this, key, value);
    }),
    spyOn(Map.prototype, "delete").mockImplementation(function (this: Map<unknown, unknown>, key) {
      if (key === "look") kept.push("delete");
      return remove.call(this, key);
    }),
  ];
  try {
    for (let i = 0; i < 3; i++) {
      for (const cmd of decode(new Encoder().timed(i, "look").finish())) executor.execute(cmd);
      await expect(executor.timeLabel("look")).rejects.toThrow("no timestamp queries");
    }
  } finally {
    for (const spy of spies) spy.mockRestore();
  }
  expect(kept).toEqual(["set [0]", "delete", "set [0]", "delete", "set [0]", "delete"]);
});

test("in the serial mode, a label's time waits for its frame's drained passes", async () => {
  const device = new FakeDevice();
  device.features.add("timestamp-query");
  const m = shapes();
  const [, compute] = await buildAll(device, m);
  const executor = new GpuExecutor(device.gpu, [compute!, compute!], SCREEN_TARGET, "serial");
  let answered: Uint8Array | null = null;
  const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
  run(new Encoder().timed(1, "look"));
  const asked = executor.timeLabel("look").then((b) => (answered = b));
  const dispatch = (e: Encoder) => e.dispatch(1, [1, 1, 1], [1, 2], u(0));
  run(dispatch(dispatch(new Encoder().createBuffer(1, 64).createBuffer(2, 64).label("look"))));
  // The drain waits after its first dispatch, while the next frame begins.
  const held = Promise.withResolvers<undefined>();
  let waits = 0;
  device.gpu.queue.onSubmittedWorkDone = () => (waits++ === 0 ? held.promise : Promise.resolve(undefined));
  const drained = executor.drain();
  executor.frame = 1;
  for (let i = 0; i < 5; i++) await Promise.resolve();
  expect(answered).toBeNull();
  held.resolve(undefined);
  await drained;
  await asked;
  expect(answered!.length).toBe(4);
  executor.destroyAll();
});

test("a shader that uses subgroups gets WGSL's `enable subgroups;`, which naga's text leaves out", () => {
  const p = (requires: string[]) => ({ requires }) as unknown as Pipeline;
  expect(shaderSource(p(["subgroups"]), "fn main() {}")).toBe("enable subgroups;\nfn main() {}");
  expect(shaderSource(p(["shader-f16"]), "enable f16;\n")).toBe("enable f16;\n");
  expect(shaderSource(p([]), "x")).toBe("x");
});

test("the executor counts what it did below the stream, until its counters are taken", async () => {
  const { executor, run } = await setup();
  const e = new Encoder().createBuffer(1, 64).createBuffer(2, 64).writeBuffer(1, 0, words([7, 8]));
  for (let i = 0; i < 3; i++) e.dispatch(1, [1, 1, 1], [1, 2], u(i));
  e.beginScreenPass([0, 0, 0, 1]);
  for (let i = 3; i < 5; i++) e.draw(0, 3, 1, [1, 2], u(i));
  run(e.present());
  expect(executor.takeCounters()).toEqual({
    // The three dispatches share a compute pass and its pipeline; each sets its bind group at
    // its own uniform offset, as each draw does. The two draws share their pipeline.
    compute_passes: 1,
    render_passes: 1,
    dispatches: 3,
    draws: 2,
    draw_calls: 2,
    // The uniform ring and the upload ring, each one write of everything staged.
    write_buffers: 2,
    // Four 256-byte uniform slots and the last draw's 16 bytes, then the upload's 8.
    write_buffer_bytes: 4 * 256 + 16 + 8,
    write_textures: 0,
    bind_groups: 5,
    pipelines: 2,
  });
  expect(executor.takeCounters().draws).toBe(0);
});

test.each([true, false])("a multi-draw is one call with the device's multi-draw (%p), else one draw each", async (multi) => {
  const device = new FakeDevice();
  if (multi) device.features.add("chromium-experimental-multi-draw-indirect" as GPUFeatureName);
  const executor = new GpuExecutor(device.gpu, await buildAll(device, shapes()), SCREEN_TARGET);
  const run = (e: Encoder) => { for (const cmd of decode(e.finish())) executor.execute(cmd); };
  run(new Encoder().createBuffer(1, 64).createBuffer(2, 64).createBuffer(3, 256).createBuffer(4, 64).beginScreenPass([0, 0, 0, 1]));
  const drawn = { pipeline: 0, bindings: [1, 2].map((handle) => ({ handle, offset: 0, size: 64 })), uniforms: u(0) };
  const multiDrawn = { ...drawn, arguments: 3, count_buffer: NONE, count_offset: 0 };
  executor.execute({ ...drawn, op: "DrawIndexed", indices: 4, index_offset: 0, index_size: 64, index_count: 6, instances: 2, first_index: 1, base_vertex: -1, first_instance: 3 });
  executor.execute({ ...multiDrawn, op: "MultiDrawIndirect", indices: null, offset: 16, count: 3 });
  executor.execute({ ...multiDrawn, op: "MultiDrawIndexedIndirect", indices: [4, 0, 64], offset: 0, count: 2 });
  run(new Encoder().present());
  const args = { pipeline: "draw", buffer: "buffer 3" };
  const expected: Event[] = [{ kind: "drawIndexed", pipeline: "draw", count: 6, instances: 2, first: 1, base: -1, firstInstance: 3, indices: "buffer 4" }];
  if (multi) {
    expected.push({ kind: "multiDrawIndirect", ...args, offset: 16, count: 3 });
    expected.push({ kind: "multiDrawIndexedIndirect", ...args, offset: 0, count: 2, indices: "buffer 4" });
  } else {
    // One draw each, at its arguments' stride: 16 bytes, or 20 indexed.
    for (const offset of [16, 32, 48]) expected.push({ kind: "drawIndirect", ...args, offset });
    for (const offset of [0, 20]) expected.push({ kind: "drawIndexedIndirect", ...args, offset, indices: "buffer 4" });
  }
  expect(device.events.filter((e) => e.kind.startsWith("draw") || e.kind.startsWith("multi"))).toEqual(expected);
  const counted = executor.takeCounters();
  expect({ draws: counted.draws, draw_calls: counted.draw_calls }).toEqual({ draws: 6, draw_calls: multi ? 3 : 6 });
});

test("a pipeline isn't made for a target the device can't blend into as it blends", async () => {
  const build = async (features: string[]) => {
    const device = new FakeDevice();
    for (const f of features) device.features.add(f as GPUFeatureName);
    const m = shapes();
    const draw = m.pipelines[0]!;
    if (draw.kind !== "render") throw new Error("the shapes' pipeline 0 renders");
    draw.blends = ["add"];
    draw.targets = [
      { colors: ["r32float"], depth: false, screen: false },
      { colors: ["rgba16float"], depth: false, screen: false },
    ];
    await buildAll(device, m);
    return device.made;
  };
  // Without float32-blendable, the r32float variant would fail the whole load.
  expect(await build([])).toEqual(["draw rgba16float|none"]);
  expect(await build(["float32-blendable"])).toEqual(["draw r32float|none", "draw rgba16float|none"]);
});

test("a pass that joins one with a scissor starts from the whole target again", async () => {
  const { device, run, executor } = await setup();
  const target = (keep: boolean): Pass => ({ ...onePass(3, keep, [0, 0, 0, 1]), join: keep });
  run(
    new Encoder()
      .createBuffer(1, 64)
      .createBuffer(2, 64)
      .createTexture(3, 8, 4, "rgba8unorm")
      .beginPass(target(false))
      .scissor(0, 0, 2, 2)
      .draw(0, 3, 1, [1, 2], u(0))
      .endPass()
      .beginPass(target(true))
      .draw(0, 3, 1, [1, 2], u(1))
      .endPass(),
  );
  executor.flush();
  const ev = device.events.filter((e) => ["renderPass", "scissor", "viewport", "draw"].includes(e.kind));
  expect(ev.map((e) => e.kind)).toEqual(["renderPass", "scissor", "draw", "scissor", "viewport", "draw"]);
  expect(ev[3]).toEqual({ kind: "scissor", rect: [0, 0, 8, 4] });
  expect(ev[4]).toEqual({ kind: "viewport", rect: [0, 0, 8, 4] });
});

describe("pipeline tiers", () => {
  test("tier 0 builds first, biggest first, and tier 1 once it has settled", async () => {
    const device = new FakeDevice();
    const m = shapes();
    m.pipelines[1] = { ...m.pipelines[1]!, conditional: true };
    const building = buildPipelines(device.gpu, m, [SHADER, SHADER], SCREEN_FORMAT);
    expect(building.order).toEqual([0]);
    await building.tier0;
    await new Promise((r) => setTimeout(r, 0));
    expect(building.order).toEqual([0, 1]);
    await Promise.all(building.each);
  });

  test("a command that needs a tier-1 pipeline starts it at once, and runs when it's built", async () => {
    const device = new FakeDevice();
    const m = shapes();
    m.pipelines[1] = { ...m.pipelines[1]!, conditional: true };
    const building = buildPipelines(device.gpu, m, [SHADER, SHADER], SCREEN_FORMAT);
    const executor = new GpuExecutor(device.gpu, building.each, SCREEN_TARGET, "off", false, building);
    for (const cmd of decode(new Encoder().createBuffer(1, 64).createBuffer(2, 64).dispatch(1, [1, 1, 1], [1, 2], u(0)).finish())) {
      executor.execute(cmd);
    }
    // Bumped before tier 0 settled.
    expect(building.order).toEqual([0, 1]);
    await executor.caughtUp();
    expect(kinds(device.events)).toContain("dispatch");
    await executor.built;
  });
});
