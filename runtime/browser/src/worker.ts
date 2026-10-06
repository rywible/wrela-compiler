// The render worker: owns the GPU device and the canvas, loads the build, and drives the
// program's `frame` — with requestAnimationFrame normally, or deterministically in test mode.

/// <reference lib="webworker" />

import { MAX_WORKERS, SCREEN_FORMAT } from "./abi.gen.ts";
import { LIMIT_SOURCES } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { fitSize, GameClock } from "./frame.ts";
import { alignTo, type GpuExecutor } from "./gpu.ts";
import { eventsAt, InputRing, parseScript, type Scripted } from "./input.ts";
import { browserIo, type Build, loadBuild, startProgram } from "./loader.ts";
import { type Program, runWorker, type SpawnWorker } from "./program.ts";
import type { FromWorker, ToWorker, VoiceOptions } from "./messages.ts";
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
/** Input events from the main thread (input.ts). */
let ring: InputRing | null = null;

/** Gives the program the events the main thread has written: it reads them in its next call.
 * Their times, for the latency test. */
function deliverInput(program: Program): number[] {
  if (!ring) return [];
  const { events, times } = ring.read();
  program.queueInput(events);
  return times;
}

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

/** The device, with timestamp queries when `timestamps` asks for them and the adapter has
 * them. */
async function openDevice(timestamps: boolean): Promise<GPUDevice> {
  const adapter = await navigator.gpu?.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) throw new GpuError("WebGPU isn't available here (no adapter)");
  // The adapter's own limits, as the native host requests them: a program asks for them
  // (`wrela.limit`), both hosts check its commands against them, and WebGPU checks what they
  // bound against them.
  const requiredLimits: Record<string, number> = {};
  for (const name of LIMIT_SOURCES.flat()) requiredLimits[name] = adapter.limits[name];
  const requiredFeatures: GPUFeatureName[] = timestamps && adapter.features.has("timestamp-query") ? ["timestamp-query"] : [];
  const device = await adapter.requestDevice({ requiredLimits, requiredFeatures });
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

/** Hands the program's voice to the main thread, which plays it in an AudioWorklet: an
 * AudioContext exists only there. */
const startVoice = (voice: VoiceOptions) => self.postMessage({ type: "audio", voice } satisfies FromWorker);

async function run(canvas: OffscreenCanvas, device: GPUDevice, build: Build): Promise<void> {
  const ctx = context(canvas, device, GPUTextureUsage.RENDER_ATTACHMENT);
  const workers = Math.min(navigator.hardwareConcurrency || 1, MAX_WORKERS + 1);
  const program = await startProgram(
    device,
    build,
    { texture: () => ctx.getCurrentTexture() },
    { io: browserIo(base), workers, spawnWorker, startVoice },
  );
  const max = device.limits.maxTextureDimension2D;
  const executor = program.executor as GpuExecutor;
  // A debug build's bounds checks: the flag is read after a frame, one read at a time.
  let checking = false;
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
        deliverInput(program);
        program.frame(time, width, height);
        if (!checking) {
          checking = true;
          executor.checkDebugFlag().then(() => (checking = false), fatal);
        }
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
  // What the program printed (`results/log.txt`), and each frame's CPU time: the program's
  // `frame` and its batch's checks and encoding, up to its submission (`results/frames.json`).
  const printed: string[] = [];
  const cpu: number[] = [];
  // When each frame began (ms since the epoch), and the frame each printed line came in.
  const began_ms: number[] = [];
  const printed_in: number[] = [];
  let frameNow = 0;
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
    { hash: params.nohash === 0, io: browserIo(base), workers: params.workers, spawnWorker, startVoice, onPrint: (line) => {
        printed.push(line);
        printed_in.push(frameNow);
      } },
    params.timestamps > 0,
  );
  const executor = program.executor as GpuExecutor;
  if (params.timestamps > 0 && !executor.timed) throw new GpuError("the test asks for timestamps, but this device has no timestamp queries");
  const hash = program.hash;
  const script: Scripted[] = params.input ? parseScript(await (await fetch(new URL(params.input, base))).text()) : [];
  // The latency test: when each frame started, and which events (by when they were sent) it got.
  const latency = { frames: [] as number[], delivered: [] as { sent: number; frame: number }[] };
  if (params.latency > 0) self.postMessage({ type: "latency-start", ms: ((frames - 2) * 1000) / fps } satisfies FromWorker);
  const start = performance.now();
  for (let i = 0; i < frames; i++) {
    // Paced at `fps`, as a display paces frames: requests are answered in real time between
    // them, as when the game runs.
    const wait = start + (i * 1000) / fps - performance.now();
    if (wait > 0) await new Promise((resolve) => setTimeout(resolve, wait));
    executor.frame = i;
    for (const e of eventsAt(script, i)) program.queueInput(e);
    latency.frames.push(performance.timeOrigin + performance.now());
    for (const sent of deliverInput(program)) latency.delivered.push({ sent, frame: i });
    const began = performance.now();
    frameNow = i;
    began_ms.push(performance.timeOrigin + began);
    await scoped(device, () => {
      program.frame(frameTime(i, fps), width, height);
      cpu.push(performance.now() - began);
    });
    await device.queue.onSubmittedWorkDone();
    await executor.checkDebugFlag();
  }
  const rgba = await readTexture(device, screen);
  await Promise.all([
    putResult(base, "frame.rgba", rgba),
    putResult(base, "frame.png", encodePng(width, height, rgba)),
    putResult(base, "hash.txt", `${hash ? hash.hex() : "none"}\n`),
    putResult(base, "workers.txt", `${program.workerChunks()}\n`),
    putResult(base, "log.txt", printed.map((l) => `${l}\n`).join("")),
    putResult(base, "frames.json", JSON.stringify({ cpu_ms: cpu, began_ms, printed_in })),
    params.timestamps > 0 ? executor.timings().then((t) => putResult(base, "timings.json", JSON.stringify(t))) : null,
  ]);
  if (params.audio > 0) {
    if (!program.hasVoice) throw new Error("the test asks for audio, but the program started no voice");
    await audioRendered;
  }
  if (params.latency > 0) {
    await latencySent;
    await putResult(base, "latency.json", JSON.stringify(latency));
  }
  console.log(`wrela test: ${frames} frames at ${width}x${height}, state hash ${hash ? hash.hex() : "none"}`);
  await putResult(base, "DONE", "ok");
}

/** Starts a worker thread for the program's parallel jobs: this script again, told to run them. */
const spawnWorker: SpawnWorker = (module, memory, index) => {
  const thread = new Worker(self.location.href, { type: "module", name: `wrela thread ${index}` });
  thread.postMessage({ type: "thread", module, memory, index } satisfies ToWorker);
};

/** Test mode: resolved when the main thread has rendered the voice and saved its samples. */
let renderedAudio = () => {};
const audioRendered = new Promise<void>((resolve) => {
  renderedAudio = resolve;
});

/** Test mode, `latency`: resolved when the main thread has sent its events. */
let sentLatency = () => {};
const latencySent = new Promise<void>((resolve) => {
  sentLatency = resolve;
});

self.onmessage = (event: MessageEvent<ToWorker>) => {
  const msg = event.data;
  switch (msg.type) {
    case "audio-rendered":
      renderedAudio();
      return;
    case "latency-sent":
      sentLatency();
      return;
    case "thread":
      // It runs until the program shuts it down, or the page goes away.
      void runWorker(msg.module, msg.memory, msg.index);
      return;
    case "start": {
      base = msg.base;
      testing = msg.test !== null;
      ring = new InputRing(msg.input);
      size = { width: msg.width, height: msg.height };
      const test = msg.test;
      // The device and the build are fetched at once.
      Promise.all([openDevice((test?.timestamps ?? 0) > 0), loadBuild(base)])
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
