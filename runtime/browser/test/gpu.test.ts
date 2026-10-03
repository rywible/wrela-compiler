// The decoder's mapping to WebGPU, on a fake device (see runtime/native/tests/suite/gpu.rs for
// the same behaviour on a real GPU).

import { describe, expect, test } from "bun:test";
import { buildPipelines, GpuExecutor, ShaderError } from "../src/gpu.ts";
import type { Manifest } from "../src/manifest.ts";
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

test("buffers are created zeroed, as storage that can be copied both ways", async () => {
  const { device, run } = await setup();
  run(new Encoder().createBuffer(3, 64));
  const b = device.buffers.find((b) => b.label === "buffer 3")!;
  expect(b.size).toBe(64);
  expect(b.usage).toBe(GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST | GPUBufferUsage.COPY_SRC);
});

test("a write after a dispatch submits the dispatch first", async () => {
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
  expect(kinds(device.events)).toEqual(["writeBuffer", "dispatch", "writeBuffer", "submit", "writeBuffer"]);
  // The first write needs no flush (nothing recorded yet); the dispatch's uniforms go into the
  // ring just before its submission.
  const [first, , ring, , second] = device.events as Extract<Event, { kind: "writeBuffer" }>[];
  expect([first!.buffer, first!.offset, Array.from(first!.data)]).toEqual(["buffer 1", 0, [7, 0, 0, 0]]);
  expect([ring!.buffer, Array.from(ring!.data)]).toEqual(["uniform ring", Array.from(u(5))]);
  expect([second!.buffer, second!.offset]).toEqual(["buffer 1", 4]);
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
    { binding: 1, buffer: "buffer 1", offset: undefined, size: undefined },
    { binding: 2, buffer: "buffer 2", offset: undefined, size: undefined },
  ]);
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
