// The render worker: owns the GPU device and the canvas, loads the build, and drives the
// program's `frame` — with requestAnimationFrame normally, or deterministically in test mode.

/// <reference lib="webworker" />

import { SCREEN_FORMAT } from "./abi.gen.ts";
import { errorMessage } from "./errors.ts";
import { fitSize, GameClock } from "./frame.ts";
import { alignTo } from "./gpu.ts";
import { type Build, loadBuild, startProgram } from "./loader.ts";
import type { FromWorker, ToWorker } from "./messages.ts";
import { encodePng } from "./png.ts";
import { frameTime, putResult, type TestParams } from "./testmode.ts";

declare const self: DedicatedWorkerGlobalScope;

/** The GPU, its adapter or device failed, or reported an error. */
class GpuError extends Error {
  constructor(why: string) {
    super(`GPU error: ${why}`);
    this.name = "GpuError";
  }
}

let base = "";
let testing = false;
let failed = false;

/** Stops everything and reports `e`: to the page, and in test mode to `results/DONE`. */
function fatal(e: unknown): void {
  if (failed) return;
  failed = true;
  const text = errorMessage(e);
  self.postMessage({ type: "fatal", message: text } satisfies FromWorker);
  if (testing) {
    putResult(base, "DONE", text).catch((err: unknown) => console.error(`can't report the failure: ${errorMessage(err)}`));
  }
}

async function openDevice(): Promise<GPUDevice> {
  const adapter = await navigator.gpu?.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) throw new GpuError("WebGPU isn't available here (no adapter)");
  // WebGPU's default limits, as the native host requests: both accept the same programs.
  const device = await adapter.requestDevice();
  device.addEventListener("uncapturederror", (e) => fatal(new GpuError(e.error.message)));
  device.lost.then((info) => {
    if (info.reason !== "destroyed") fatal(new GpuError(`device lost: ${info.message}`));
  });
  return device;
}

function context(canvas: OffscreenCanvas, device: GPUDevice, usage: number): GPUCanvasContext {
  const ctx = canvas.getContext("webgpu");
  if (!ctx) throw new GpuError("the canvas has no WebGPU context");
  ctx.configure({ device, format: SCREEN_FORMAT, alphaMode: "opaque", usage });
  return ctx;
}

// ---- Running normally ----

let size = { width: 1, height: 1 };
const clock = new GameClock();

async function run(canvas: OffscreenCanvas, device: GPUDevice, build: Build): Promise<void> {
  const ctx = context(canvas, device, GPUTextureUsage.RENDER_ATTACHMENT);
  const program = await startProgram(device, build, { texture: () => ctx.getCurrentTexture() });
  const max = device.limits.maxTextureDimension2D;
  const tick = (now: number) => {
    if (failed) return;
    try {
      const time = clock.tick(now);
      if (time !== null) {
        const { width, height } = fitSize(size.width, size.height, max);
        if (canvas.width !== width || canvas.height !== height) {
          canvas.width = width;
          canvas.height = height;
        }
        program.frame(time, width, height);
      }
      self.requestAnimationFrame(tick);
    } catch (e) {
      fatal(e);
    }
  };
  self.requestAnimationFrame(tick);
}

// ---- Test mode ----

/** Runs inside error scopes, so a GPU error fails the test at the frame that caused it. */
async function scoped(device: GPUDevice, f: () => void): Promise<void> {
  const filters: GPUErrorFilter[] = ["validation", "out-of-memory", "internal"];
  for (const filter of filters) device.pushErrorScope(filter);
  let thrown: unknown = null;
  try {
    f();
  } catch (e) {
    thrown = e;
  }
  const errors = await Promise.all(filters.map(() => device.popErrorScope()));
  if (thrown !== null) throw thrown;
  const error = errors.find((e) => e !== null);
  if (error) throw new GpuError(error.message);
}

/** Copies a texture back: RGBA8, rows top to bottom, no padding. */
async function readTexture(device: GPUDevice, texture: GPUTexture): Promise<Uint8Array<ArrayBuffer>> {
  const { width, height } = texture;
  const row = width * 4;
  const padded = alignTo(row, 256);
  const staging = device.createBuffer({ size: padded * height, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
  const encoder = device.createCommandEncoder();
  encoder.copyTextureToBuffer({ texture }, { buffer: staging, bytesPerRow: padded, rowsPerImage: height }, [width, height]);
  device.queue.submit([encoder.finish()]);
  await staging.mapAsync(GPUMapMode.READ);
  const mapped = new Uint8Array(staging.getMappedRange());
  const out = new Uint8Array(row * height);
  for (let y = 0; y < height; y++) out.set(mapped.subarray(y * padded, y * padded + row), y * row);
  staging.unmap();
  staging.destroy();
  return out;
}

async function runTest(canvas: OffscreenCanvas, device: GPUDevice, build: Build, params: TestParams): Promise<void> {
  const { frames, width, height, fps } = params;
  canvas.width = width;
  canvas.height = height;
  const ctx = context(canvas, device, GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_DST);
  // Screen passes render into a texture that can be read back; each is also copied to the
  // canvas so the page shows what ran.
  const screen = device.createTexture({
    label: "screen",
    size: [width, height],
    format: SCREEN_FORMAT,
    usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
  });
  const program = await startProgram(
    device,
    build,
    {
      texture: () => screen,
      afterPass: (encoder) =>
        encoder.copyTextureToTexture({ texture: screen }, { texture: ctx.getCurrentTexture() }, [width, height]),
    },
    { hash: true },
  );
  const hash = program.hash!;
  for (let i = 0; i < frames; i++) {
    await scoped(device, () => program.frame(frameTime(i, fps), width, height));
    await device.queue.onSubmittedWorkDone();
  }
  const rgba = await readTexture(device, screen);
  await Promise.all([
    putResult(base, "frame.rgba", rgba),
    putResult(base, "frame.png", encodePng(width, height, rgba)),
    putResult(base, "hash.txt", `${hash.hex()}\n`),
  ]);
  console.log(`wrela test: ${frames} frames at ${width}x${height}, state hash ${hash.hex()}`);
  await putResult(base, "DONE", "ok");
}

self.onmessage = (event: MessageEvent<ToWorker>) => {
  const msg = event.data;
  switch (msg.type) {
    case "start": {
      base = msg.base;
      testing = msg.test !== null;
      size = { width: msg.width, height: msg.height };
      const test = msg.test;
      // The device and the build are fetched at once.
      Promise.all([openDevice(), loadBuild(base)])
        .then(([device, build]) => (test ? runTest(msg.canvas, device, build, test) : run(msg.canvas, device, build)))
        .catch(fatal);
      return;
    }
    case "resize":
      size = { width: msg.width, height: msg.height };
      return;
    case "visibility":
      clock.setVisible(msg.visible);
      return;
  }
};
