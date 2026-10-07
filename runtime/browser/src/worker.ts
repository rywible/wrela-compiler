// The render worker: owns the GPU device and the canvas, loads the build, and drives the
// program's `frame` — with requestAnimationFrame normally, or deterministically in test mode.

/// <reference lib="webworker" />

import { MAX_WORKERS, SCREEN_FORMAT } from "./abi.gen.ts";
import { LIMIT_SOURCES } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { fitSize, frameSeconds, VisibleClock } from "./frame.ts";
import { alignTo, type GpuExecutor } from "./gpu.ts";
import { arrivals, epochNow, InputRing, parseScript, type Scripted } from "./input.ts";
import { browserIo, type Build, loadBuild, startProgram } from "./loader.ts";
import { type Program, runWorker, type SpawnWorker } from "./program.ts";
import type { FromTicker, FromWorker, Loaded, TickerOptions, TickerStart, TickReport, ToWorker, VoiceOptions } from "./messages.ts";
import { encodePng } from "./png.ts";
import { frameTime, holdFor, loaded, putResult, sleepUntil, type TestParams } from "./testmode.ts";
import { lockstepTicks, runTicker, TickerControl, TickerMode } from "./ticker.ts";
import { wasmHash } from "./ticks.ts";

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
function deliverInput(program: Program): readonly number[] {
  if (!ring) return [];
  const { events, times } = ring.read();
  program.queueInput(events);
  return times;
}

/** Stops everything and reports `e`: to the page, and in test mode to `results/DONE`. */
function fatal(e: unknown): void {
  if (failed) return;
  failed = true;
  ticker.control.stop();
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

// ---- The ticker's thread ----

/** Visible time, which frames and ticks count (frame.ts). */
const clock = VisibleClock.create();

/** The ticker's thread, once the program starts its ticker. */
const ticker = {
  control: TickerControl.create(),
  /** Test mode: resolved with the ticks when the ticker's thread has stopped. */
  report: null as Promise<{ report: TickReport; log: Uint8Array<ArrayBuffer> | null }> | null,
};

/** How the ticker's thread runs the ticks: test mode's settings, or none (`NORMAL_TICKS`). */
type TickSettings = Pick<TickerStart, "script" | "hashes" | "timing" | "delay">;

/** A normal run's: no script, nothing kept, no delay. */
const NORMAL_TICKS: TickSettings = { script: [], hashes: false, timing: false, delay: 0 };

/** What starts the program's ticker on its own thread, with `settings`: this script again, told
 * to run it. `wasm` is the build's, whose hash heads the tick log when hashes are kept. */
function tickerStarter(wasm: Uint8Array, settings: TickSettings): (options: TickerOptions) => void {
  return (options) => {
    ring?.attachTicker();
    const thread = new Worker(self.location.href, { type: "module", name: "wrela sim" });
    ticker.report = new Promise((resolve) => {
      thread.onmessage = (event: MessageEvent<FromTicker>) => {
        const msg = event.data;
        if (msg.type === "fatal") fatal(new Error(msg.message));
        else resolve(msg);
      };
    });
    thread.onerror = (event) => {
      event.preventDefault();
      fatal(new Error(`the ticker's thread failed: ${event.message || "it couldn't start"}`));
    };
    thread.postMessage({
      type: "ticker",
      ...options,
      ...settings,
      control: ticker.control.buffer,
      clock: clock.buffer,
      input: ring!.buffer,
      wasmHash: settings.hashes ? wasmHash(wasm) : 0n,
    } satisfies ToWorker);
  };
}

// ---- Running normally ----

let size = { width: 1, height: 1 };

/** Hands the program's voice to the main thread, which plays it in an AudioWorklet: an
 * AudioContext exists only there. */
const startVoice = (voice: VoiceOptions) => self.postMessage({ type: "audio", voice } satisfies FromWorker);

/** Helpers for parallel work and jobs: one per core beyond the main thread, the render worker
 * and the ticker's thread, at most MAX_WORKERS (#43 §2.1). The program's own thread counts too. */
const normalWorkers = () => Math.max(0, Math.min((navigator.hardwareConcurrency || 1) - 3, MAX_WORKERS)) + 1;

async function run(canvas: OffscreenCanvas, device: GPUDevice, build: Build): Promise<void> {
  const ctx = context(canvas, device, GPUTextureUsage.RENDER_ATTACHMENT);
  clock.startAt(epochNow());
  const program = await startProgram(
    device,
    build,
    { texture: () => ctx.getCurrentTexture() },
    {
      io: browserIo(base),
      workers: normalWorkers(),
      spawnWorker,
      startVoice,
      startTicker: tickerStarter(build.wasm, NORMAL_TICKS),
    },
  );
  ticker.control.go(TickerMode.CLOCK);
  const max = device.limits.maxTextureDimension2D;
  const executor = program.executor as GpuExecutor;
  // A debug build's bounds checks: the flag is read after a frame, one read at a time.
  let checking = false;
  const tick = (now: number) => {
    if (failed) return;
    try {
      const time = frameSeconds(clock, performance.timeOrigin + now);
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
  const script: Scripted[] = params.input ? parseScript(await (await fetch(new URL(params.input, base))).text()) : [];
  const frameEvents = arrivals(script).frames;
  const hashes = params.nohash === 0;
  let pipelinesMs: number | null = null;
  const salted =
    params.salt > 0
      ? { ...build, shaders: build.shaders.map((s, i) => `${s}\n// salt ${params.salt} ${i}\n`) }
      : build;
  const program = await startProgram(
    device,
    salted,
    {
      texture: () => screen,
      afterPass: (encoder) =>
        encoder.copyTextureToTexture({ texture: screen }, { texture: ctx.getCurrentTexture() }, [width, height]),
    },
    {
      hash: hashes,
      io: browserIo(base),
      workers: params.workers,
      spawnWorker,
      startVoice,
      startTicker: tickerStarter(build.wasm, { script, hashes, timing: true, delay: params.tickdelay }),
      tickHashes: hashes,
      onPrint: (line) => {
        printed.push(line);
        printed_in.push(frameNow);
      },
    },
    params.timestamps > 0,
    (ms) => {
      pipelinesMs = ms;
    },
  );
  const executor = program.executor as GpuExecutor;
  if (params.timestamps > 0 && !executor.timed) throw new GpuError("the test asks for timestamps, but this device has no timestamp queries");
  const hash = program.hash;
  const hz = program.ticker?.hz ?? 0;
  const control = ticker.control;
  // The latency test: when each frame started, and which events (by when they were sent) it got.
  const latency = { frames: [] as number[], delivered: [] as { sent: number; frame: number }[] };
  if (params.latency > 0 || params.keylatency > 0) {
    self.postMessage({ type: "latency-start", ms: ((frames - 2) * 1000) / fps } satisfies FromWorker);
  }
  const start = performance.now();
  if (hz > 0) {
    clock.startAt(epochNow());
    control.go(params.paced > 0 ? TickerMode.CLOCK : TickerMode.LOCKSTEP);
  }
  for (let i = 0; i < frames; i++) {
    // Paced at `fps`, as a display paces frames: requests are answered in real time between
    // them, as when the game runs.
    await sleepUntil(start + (i * 1000) / fps);
    executor.frame = i;
    control.framesStarted(i + 1);
    if (hz > 0 && params.paced === 0) {
      // Lockstep: the ticks before this frame run first, as on the native host.
      const target = lockstepTicks(i, hz, fps);
      control.runTo(target);
      await control.ranTo(target);
      // The ticker's thread reports its trap (`tickerStarter`).
      if (control.failed) return;
    }
    for (const e of frameEvents.get(i) ?? []) program.queueInput(e);
    latency.frames.push(epochNow());
    for (const sent of deliverInput(program)) latency.delivered.push({ sent, frame: i });
    const began = performance.now();
    frameNow = i;
    began_ms.push(performance.timeOrigin + began);
    await scoped(device, () => {
      program.frame(frameTime(i, fps), width, height);
      if (params.framedelay > 0) holdFor(params.framedelay);
      cpu.push(performance.now() - began);
    });
    await device.queue.onSubmittedWorkDone();
    await executor.checkDebugFlag();
  }
  if (hz > 0) {
    control.stop();
    const { report, log } = await ticker.report!;
    await Promise.all([
      putResult(base, "ticks.json", JSON.stringify({ hz, ...report })),
      log ? putResult(base, "ticks.log", log) : null,
    ]);
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
    params.salt > 0 ? putResult(base, "pipelines.json", JSON.stringify({ ms: pipelinesMs, count: build.manifest.pipelines.length })) : null,
    putResult(base, "memory.json", JSON.stringify({ gpu: executor.memory, wasm: program.memoryBytes })),
  ]);
  if (params.audio > 0) {
    if (!program.hasVoice) throw new Error("the test asks for audio, but the program started no voice");
    await audioRendered.promise;
  }
  if (params.latency > 0 || params.keylatency > 0) {
    await latencySent.promise;
    await putResult(base, "latency.json", JSON.stringify(latency));
  }
  self.postMessage({ type: "load-query" } satisfies FromWorker);
  const page = await pageLoaded.promise;
  const resources = [...page.resources, ...loaded(["resource"])];
  const load = { opened_ms: page.opened_ms, first_frame_ms: began_ms[0] ?? null, resources };
  await putResult(base, "load.json", JSON.stringify(load));
  console.log(`wrela test: ${frames} frames at ${width}x${height}, state hash ${hash ? hash.hex() : "none"}`);
  await putResult(base, "DONE", "ok");
}

/** Starts a worker thread for the program's parallel jobs: this script again, told to run them. */
const spawnWorker: SpawnWorker = (module, memory, index) => {
  const thread = new Worker(self.location.href, { type: "module", name: `wrela thread ${index}` });
  thread.postMessage({ type: "thread", module, memory, index } satisfies ToWorker);
};

/** Test mode: resolved when the main thread has rendered the voice and saved its samples. */
const audioRendered = Promise.withResolvers<void>();

/** Test mode, `latency`: resolved when the main thread has sent its events. */
const latencySent = Promise.withResolvers<void>();

/** Test mode: resolved with what the page loaded, when the main thread answers. */
const pageLoaded = Promise.withResolvers<{ opened_ms: number; resources: Loaded[] }>();

self.onmessage = (event: MessageEvent<ToWorker>) => {
  const msg = event.data;
  switch (msg.type) {
    case "load":
      pageLoaded.resolve(msg);
      return;
    case "ticker": {
      // This is the ticker's thread: it runs until it's stopped, or the page goes away.
      const { type: _, ...start } = msg;
      void runTicker(start, (m) => self.postMessage(m));
      return;
    }
    case "audio-rendered":
      audioRendered.resolve();
      return;
    case "latency-sent":
      latencySent.resolve();
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
      clock.setVisible(msg.visible, epochNow());
      // The ticker's thread sleeps while the page is hidden.
      ticker.control.wake();
      return;
  }
};
