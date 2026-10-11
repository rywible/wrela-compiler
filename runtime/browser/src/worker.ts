// The render worker: owns the GPU device and the canvas, loads the build, and drives the
// program's `frame` — with requestAnimationFrame normally, or deterministically in test mode.

/// <reference lib="webworker" />

import { MAX_WORKERS, SCREEN_FORMAT } from "./abi.gen.ts";
import { LIMIT_SOURCES } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { fitSize, frameSeconds, VisibleClock } from "./frame.ts";
import { alignTo, type GpuExecutor, type PipelineCache, type Timing, timeEachPipeline } from "./gpu.ts";
import { arrivals, epochNow, InputRing, parseScript, type Scripted } from "./input.ts";
import { browserIo, type Build, loadBuild, startProgram } from "./loader.ts";
import { Live } from "./live.ts";
import { type Program, type ProgramOptions, runWorker, type SpawnWorker } from "./program.ts";
import type { FromTicker, FromWorker, Loaded, Replay, TickerOptions, TickerStart, TickReport, ToWorker, VoiceOptions } from "./messages.ts";
import { encodePng } from "./png.ts";
import { frameTime, holdFor, loaded, putResult, sleepUntil, type TestParams } from "./testmode.ts";
import { lockstepTicks, runTicker, TickerControl, TickerMode } from "./ticker.ts";
import { wasmHash } from "./ticks.ts";

declare const self: DedicatedWorkerGlobalScope;

/** `build` with code no cache has seen: each pipeline's entry points start by storing `n` in a
 * variable of their own, so the shaders the GPU's compiler gets differ from any it compiled
 * before. A comment or a new name wouldn't do: the WGSL compiler drops comments and renames
 * every symbol, so its output, which the GPU's compiler caches by, would be the same. */
function salt(build: Build, n: number): Build {
  const shaders = build.shaders.map((wgsl, i) => {
    const p = build.manifest.pipelines[i]!;
    const entries = p.kind === "compute" ? [p.entry] : [p.vertex_entry, p.fragment_entry];
    let out = wgsl;
    for (const entry of entries) {
      const at = out.search(new RegExp(`\\bfn ${entry}\\(`));
      const body = at < 0 ? -1 : out.indexOf("{", at);
      if (body < 0) throw new Error(`pipeline ${i} declares no entry point \`${entry}\``);
      out = `${out.slice(0, body + 1)}\n    wrela_salt = ${n}u;${out.slice(body + 1)}`;
    }
    return `${out}\nvar<private> wrela_salt: u32;\n`;
  });
  return { ...build, shaders };
}

/** The GPU, its adapter or device failed, or reported an error. */
class GpuError extends Error {
  constructor(why: string) {
    super(`GPU error: ${why}`);
    this.name = "GpuError";
  }
}

let base = "";
/** The build that's running's base: the page's, until a hot reload swaps another in. Its files
 * are fetched from there (the server deletes old builds, the page's among them). */
let buildBase = "";
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

/** The ticker's thread, once the program starts its ticker. A hot reload gives the new
 * build's ticker a new one. */
const ticker = {
  control: TickerControl.create(),
  /** Resolved with the ticks when the ticker's thread has stopped (test mode, a hot reload). */
  report: null as Promise<{ report: TickReport; log: Uint8Array<ArrayBuffer> | null; replay: Replay }> | null,
  thread: null as Worker | null,
};

/** How the ticker's thread runs the ticks: test mode's settings, or none (`NORMAL_TICKS`), and
 * whether the page is live. */
type TickSettings = Pick<TickerStart, "script" | "hashes" | "timing" | "delay" | "replayable">;

/** A normal run's: no script, nothing kept, no delay. */
const NORMAL_TICKS: TickSettings = { script: [], hashes: false, timing: false, delay: 0, replayable: false };

/** What starts the program's ticker on its own thread, with `settings`: this script again, told
 * to run it. `wasm` is the build's, whose hash heads the tick log when hashes are kept. With
 * `replay`, a hot reload's: the ticker runs the replaced build's ticks again first, and reads
 * the input ring on from where the replaced one stopped. */
function tickerStarter(wasm: Uint8Array, settings: TickSettings, replay: Replay | null = null): (options: TickerOptions) => void {
  return (options) => {
    if (replay === null) ring?.attachTicker();
    const thread = new Worker(self.location.href, { type: "module", name: "wrela sim" });
    ticker.thread = thread;
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
      replay,
    } satisfies ToWorker);
  };
}

// ---- Hot reload ----

/** The running build's pipelines: the next build keeps those that are the same. */
const pipelineCache: PipelineCache = new Map();

/** The helper threads the running program started: a hot reload stops them. */
const helpers: Worker[] = [];

/** What a replaced program hands on to its new build: what it kept, its input not yet read,
 * and its ticks to run again. */
interface Carried {
  kept: Uint8Array;
  input: Uint8Array[];
  replay: Replay | null;
}

/** The options both ways of running give the program of build `b`: its requests, helpers,
 * voice and ticker (with `ticks`' settings), and what the build it replaces, if it does, hands
 * on (`carried`). */
function programOptions(b: Build, ticks: TickSettings, carried: Carried | null): ProgramOptions {
  return {
    io: browserIo(base, undefined, buildBase),
    spawnWorker,
    startVoice,
    startTicker: tickerStarter(b.wasm, ticks, carried?.replay ?? null),
    kept: carried?.kept,
    input: carried?.input,
  };
}

/** Stops `old` for the build at `base` (hot reload): its ticker stops and reports its ticks,
 * its helpers stop, and its buffers and textures go. `start` starts the new build with what
 * the old one hands on; the ticker's new words say `frames` frames have started. */
async function swap(
  old: Program,
  base: string,
  frames: number,
  start: (build: Build, carried: Carried) => Promise<Program>,
): Promise<Program> {
  const build = await loadBuild(base);
  buildBase = base;
  let replay: Replay | null = null;
  if (old.ticker !== null && ticker.report !== null) {
    const stopped = ticker.report;
    ticker.control.stop();
    replay = (await stopped).replay;
    ticker.thread?.terminate();
  }
  old.shutdown();
  for (const h of helpers.splice(0)) h.terminate();
  (old.executor as GpuExecutor).destroyAll();
  ticker.control = TickerControl.create();
  ticker.report = null;
  ticker.thread = null;
  ticker.control.framesStarted(frames);
  return start(build, { kept: old.keeping, input: old.unreadInput, replay });
}

// ---- Running normally ----

let size = { width: 1, height: 1 };

/** Hands the program's voice to the main thread, which plays it in an AudioWorklet: an
 * AudioContext exists only there. */
const startVoice = (voice: VoiceOptions) => self.postMessage({ type: "audio", voice } satisfies FromWorker);

/** Helpers for parallel work and jobs: one per core beyond the main thread, the render worker
 * and the ticker's thread, at most MAX_WORKERS (#43 §2.1). The program's own thread counts too. */
const normalWorkers = () => Math.max(0, Math.min((navigator.hardwareConcurrency || 1) - 3, MAX_WORKERS)) + 1;

async function run(canvas: OffscreenCanvas, device: GPUDevice, build: Build, live: Live | null): Promise<void> {
  const ctx = context(canvas, device, GPUTextureUsage.RENDER_ATTACHMENT);
  clock.startAt(epochNow());
  const screen = { texture: () => ctx.getCurrentTexture() };
  const start = (b: Build, carried: Carried | null) =>
    startProgram(
      device,
      b,
      screen,
      { ...programOptions(b, { ...NORMAL_TICKS, replayable: live !== null }, carried), workers: normalWorkers() },
      { cache: live ? pipelineCache : undefined },
    );
  let program = await start(build, null);
  (program.executor as GpuExecutor).built.catch(fatal);
  ticker.control.go(TickerMode.CLOCK);
  const max = device.limits.maxTextureDimension2D;
  // A debug build's bounds checks: the flag is read after a frame, one read at a time.
  let checking = false;
  // Hot reload: the frames drawn, and whether a new build is being swapped in (the canvas
  // keeps the last frame meanwhile).
  let frames = 0;
  let swapping = false;
  const tick = (now: number) => {
    if (failed) return;
    try {
      const next = live && !swapping ? live.take(program) : null;
      if (next !== null) {
        // A voice plays on the main thread, which a swap can't reach: the page reloads.
        if (program.hasVoice) {
          self.postMessage({ type: "reload" } satisfies FromWorker);
          return;
        }
        swapping = true;
        swap(program, next, frames, start).then((p) => {
          program = p;
          (p.executor as GpuExecutor).built.catch(fatal);
          ticker.control.go(TickerMode.CLOCK);
          live!.swapped();
          swapping = false;
        }, fatal);
      }
      const executor = program.executor as GpuExecutor;
      // A frame waits while the last one's commands wait for a pipeline being built (the
      // program starts as its pipelines build): the canvas keeps the last frame meanwhile.
      if (swapping || executor.waiting) {
        self.requestAnimationFrame(tick);
        return;
      }
      const time = frameSeconds(clock, performance.timeOrigin + now);
      if (time !== null) {
        const { width, height } = fitSize(size.width, size.height, max);
        if (canvas.width !== width || canvas.height !== height) {
          canvas.width = width;
          canvas.height = height;
        }
        deliverInput(program);
        executor.presented = false;
        executor.frame = frames;
        program.frame(time, width, height);
        // A change is shown by the first frame that draws after it.
        if (executor.presented) live?.drawn(frames);
        frames++;
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

/** Runs inside error scopes, so a GPU error fails the test at the frame that caused it (and an
 * error `f` throws, or its promise rejects with, fails it too). */
async function scoped(device: GPUDevice, f: () => void | Promise<void>): Promise<void> {
  const filters: GPUErrorFilter[] = ["validation", "out-of-memory", "internal"];
  for (const filter of filters) device.pushErrorScope(filter);
  let thrown: unknown = null;
  try {
    await f();
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

async function runTest(canvas: OffscreenCanvas, device: GPUDevice, build: Build, params: TestParams, live: Live | null): Promise<void> {
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
  // The tick log needs only the ticks' hashes (the sim's state, cheap), not the frames'.
  const tickHashes = hashes || params.ticklog > 0;
  let pipelinesMs: number | null = null;
  const salted = params.salt > 0 ? salt(build, params.salt) : build;
  const target = {
    texture: () => screen,
      // Saturated, the canvas is left alone: it shows a frame per display refresh, and would
      // pace the frames by the display's.
    afterPass: (encoder: GPUCommandEncoder) => {
      if (params.saturate === 0) {
        encoder.copyTextureToTexture({ texture: screen }, { texture: ctx.getCurrentTexture() }, [width, height]);
      }
    },
  };
  // `timestamps=1` times each pass; `timestamps=2` runs each alone too (testmode.ts).
  const timing: Timing = params.timestamps === 2 ? "serial" : params.timestamps > 0 ? "span" : "off";
  const start = (b: Build, carried: Carried | null) =>
    startProgram(
      device,
      b,
      target,
      {
        ...programOptions(
          b,
          { script, hashes: tickHashes, timing: true, delay: params.tickdelay, replayable: live !== null },
          carried,
        ),
        hash: hashes,
        workers: params.workers,
        tickHashes,
        onPrint: (line) => {
          printed.push(line);
          printed_in.push(frameNow);
        },
      },
      {
        timing,
        onPipelines: (ms) => {
          pipelinesMs = ms;
        },
        cache: live ? pipelineCache : undefined,
      },
    );
  let program = await start(salted, null);
  let executor = program.executor as GpuExecutor;
  executor.built.catch(fatal);
  if (timing !== "off" && !executor.timed) throw new GpuError("the test asks for timestamps, but this device has no timestamp queries");
  let hz = program.ticker?.hz ?? 0;
  const schedule = params.paced > 0 ? TickerMode.CLOCK : TickerMode.LOCKSTEP;
  // The latency test: when each frame started, and which events (by when they were sent) it got.
  const latency = { frames: [] as number[], delivered: [] as { sent: number; frame: number }[] };
  if (params.latency > 0 || params.keylatency > 0) {
    self.postMessage({ type: "latency-start", ms: ((frames - 2) * 1000) / fps } satisfies FromWorker);
  }
  const began0 = performance.now();
  if (hz > 0) {
    clock.startAt(epochNow());
    ticker.control.go(schedule);
  }
  // Saturated, the frames the GPU hasn't finished (at most two: the CPU records a frame while
  // the GPU draws the last).
  const inFlight: Promise<undefined>[] = [];
  // The serial timing mode's runs of each frame's passes, one after another, so their commands
  // keep their order; the next frame is recorded while one runs, as a GPU-bound game's would
  // be (waiting for each idled the GPU between frames, and its clock fell).
  let draining: Promise<void> = Promise.resolve();
  const drains: Promise<void>[] = [];
  for (let i = 0; i < frames; i++) {
    // Paced at `fps`, as a display paces frames: requests are answered in real time between
    // them, as when the game runs.
    if (params.saturate === 0) await sleepUntil(began0 + (i * 1000) / fps);
    // Hot reload: a new build replaces the program before this frame.
    const next = live?.take(program) ?? null;
    if (next !== null) {
      program = await swap(program, next, i, start);
      executor = program.executor as GpuExecutor;
      executor.built.catch(fatal);
      hz = program.ticker?.hz ?? 0;
      if (hz > 0) ticker.control.go(schedule);
      live!.swapped();
    }
    const control = ticker.control;
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
    if (params.clip > 0 && i === params.clipfrom) self.postMessage({ type: "clip-start" } satisfies FromWorker);
    if (params.clip > 0 && i === params.clipfrom + params.clip) self.postMessage({ type: "clip-stop" } satisfies FromWorker);
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
    // The frame's commands that wait for a pipeline being built run once it is.
    await executor.caughtUp();
    if (timing === "serial") {
      // The serial timing mode runs the frame's passes now, one at a time.
      draining = draining.then(() => scoped(device, () => executor.drain()));
      draining.catch(fatal);
      // At most two frames' passes waiting to run: recording stays near the GPU.
      drains.push(draining);
      if (drains.length > 2) await drains.shift()!.catch(() => {});
    }
    if (params.saturate > 0 || params.inflight > 0) {
      inFlight.push(device.queue.onSubmittedWorkDone());
      if (inFlight.length > (params.saturate > 0 ? 2 : params.inflight)) await inFlight.shift();
    } else await device.queue.onSubmittedWorkDone();
    await executor.checkDebugFlag();
    if (executor.presented) live?.drawn(i);
    executor.presented = false;
    // `snap=n`: the screen every n frames from `snapfrom`, a PNG each (`snap-<frame>.png`).
    if (params.snap > 0 && i >= params.snapfrom && (i - params.snapfrom) % params.snap === 0) {
      await putResult(base, `snap-${i}.png`, encodePng(width, height, await readTexture(device, screen)));
    }
  }
  await draining;
  // Where the pipelines' time went: each built again alone, from code no cache has seen.
  const again = params.salt > 0 ? salt(build, params.salt + 7919) : null;
  const each = again ? await timeEachPipeline(device, again.manifest, again.shaders) : [];
  const hash = program.hash;
  if (hz > 0) {
    ticker.control.stop();
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
    timing !== "off" ? executor.timings().then((t) => putResult(base, "timings.json", JSON.stringify(t))) : null,
    params.salt > 0 ? putResult(base, "pipelines.json", JSON.stringify({ ms: pipelinesMs, count: build.manifest.pipelines.length, each })) : null,
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
  if (params.clip > 0) {
    if (params.clipfrom + params.clip >= frames) self.postMessage({ type: "clip-stop" } satisfies FromWorker);
    await clipSaved.promise;
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
  helpers.push(thread);
  thread.postMessage({ type: "thread", module, memory, index } satisfies ToWorker);
};

/** Test mode: resolved when the main thread has rendered the voice and saved its samples. */
const audioRendered = Promise.withResolvers<void>();

/** Test mode, `latency`: resolved when the main thread has sent its events. */
const latencySent = Promise.withResolvers<void>();

/** Test mode, `clip`: resolved when the main thread has saved the clip. */
const clipSaved = Promise.withResolvers<void>();

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
    case "clip-saved":
      clipSaved.resolve();
      return;
    case "thread":
      // It runs until the program shuts it down, or the page goes away.
      void runWorker(msg.module, msg.memory, msg.index);
      return;
    case "start": {
      base = msg.base;
      buildBase = msg.base;
      testing = msg.test !== null;
      ring = new InputRing(msg.input);
      size = { width: msg.width, height: msg.height };
      const test = msg.test;
      // The device and the build are fetched at once.
      const live = msg.live !== null ? new Live(base, msg.live) : null;
      live?.start();
      Promise.all([openDevice((test?.timestamps ?? 0) > 0), loadBuild(base)])
        .then(([device, build]) => (test ? runTest(msg.canvas, device, build, test, live) : run(msg.canvas, device, build, live)))
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
