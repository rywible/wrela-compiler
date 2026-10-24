// The render worker: owns the GPU device and the canvas, loads the build, and drives the
// program's `frame` — with requestAnimationFrame normally, or deterministically in test mode.

/// <reference lib="webworker" />

import { MAX_WORKERS, SCREEN_FORMAT, TICK_ORIGIN } from "./abi.gen.ts";
import { GpuError, openDevice } from "./device.ts";
import { errorMessage, UnsupportedError } from "./errors.ts";
import { fitSize, frameSeconds, VisibleClock } from "./frame.ts";
import { alignTo, type GpuExecutor, type PipelineCache, screenFormat, type ScreenTarget, type Timing, timeEachPipeline } from "./gpu.ts";
import { arrivals, epochNow, InputRing, type Scripted } from "./input.ts";
import { type Asked, HandOvers } from "./handover.ts";
import { browserIo, type Build, Handed, loadBuild, loadLines, loadScript, startProgram } from "./loader.ts";
import { locate } from "./lines.ts";
import { Live } from "./live.ts";
import { type Inbox, Net } from "./net.ts";
import { type PrintSink, type Program, type ProgramOptions, runWorker, type SpawnWorker } from "./program.ts";
import type { FromHelper, FromTicker, FromWorker, Loaded, Replay, TickerOptions, TickerStart, TickReport, ToWorker, VoiceOptions } from "./messages.ts";
import { encodePng } from "./png.ts";
import { Resampler } from "./resample.ts";
import { frameTime, holdFor, inputUrl, loaded, putResult, sleepUntil, type TestParams } from "./testmode.ts";
import { lockstepTicks, runTicker, TickerControl, TickerMode } from "./ticker.ts";
import { wasmHash } from "./ticks.ts";
import { failAudioRender } from "./worker-audio.ts";

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

let base = "";
/** The build that's running's base: the page's, until a hot reload swaps another in. Its files
 * are fetched from there (the server deletes old builds, the page's among them). */
let buildBase = "";
let testing = false;
/** The build's files the page fetched for this worker (loader.ts `Handed`). */
let handed = new Handed([]);
let failed = false;
/** Input events from the main thread (input.ts). */
let ring: InputRing | null = null;

/** The program's connections: one set for the page, which a reload to another program closes.
 * Its server is in `net.json` beside the running build. */
const net = new Net(
  () => buildBase,
  (line) => {
    console.warn(`wrela: ${line}`);
    hostLine?.(line);
  },
);

/** Gives the program the events the main thread has written, and what its connections had: it
 * reads them in its next call. A frame shown at `frame` (ms since 1970) gets a held pointer's moves
 * resampled to it (resample.ts); the latency test's, as they came. The input's times, for that
 * test. */
function deliverInput(program: Program, frame: number | null = null): readonly number[] {
  net.deliver(program.inbox);
  if (!ring) return [];
  const { events, times } = ring.read();
  program.queueInput(frame === null ? events : resampler.take(events, times, frame));
  return times;
}

const resampler = new Resampler();

/** Completion writes in order: a failure that arrives during a successful PUT must be the
 * server's final result, even when that first PUT is slow. */
let doneReport = Promise.resolve();
function reportDone(text: string): Promise<void> {
  doneReport = doneReport.catch(() => {}).then(() => putResult(base, "DONE", text));
  return doneReport;
}

/** Stops everything and reports `e`: to the page, and in test mode to `results/DONE`. A trap in
 * an optimized build is reported once its location is fetched (`locate`). */
function fatal(e: unknown): void {
  if (failed) return;
  failed = true;
  ticker.control.stop();
  const build = running;
  const at = buildBase;
  void locate(errorMessage(e), async () => (build ? loadLines(at, build.manifest) : null)).then((text) => {
    self.postMessage({ type: "fatal", message: text, unsupported: e instanceof UnsupportedError } satisfies FromWorker);
    if (testing) {
      reportDone(text).catch((err: unknown) => console.error(`can't report the failure: ${errorMessage(err)}`));
    }
  });
}

let storageNamespace: string | null = null;
let nextHostRequest = 1;
const hostRequests = new Map<number, { resolve: (bytes: Uint8Array<ArrayBuffer>) => void; reject: (e: Error) => void }>();
function requestHost(service: string, bytes: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
  const id = nextHostRequest++;
  return new Promise((resolve, reject) => {
    hostRequests.set(id, { resolve, reject });
    self.postMessage({ type: "host-request", id, service, bytes } satisfies FromWorker);
  });
}

/** The canvas's WebGPU context, configured in `format`. */
function context(canvas: OffscreenCanvas, device: GPUDevice, format: GPUTextureFormat, usage: number): GPUCanvasContext {
  const ctx = canvas.getContext("webgpu");
  if (!ctx) throw new GpuError("the canvas has no WebGPU context");
  ctx.configure({ device, format, alphaMode: "opaque", usage });
  return ctx;
}

// ---- The ticker's thread ----

/** Visible time, which frames and ticks count (frame.ts). */
const clock = VisibleClock.create();

/** A thread of the render worker's: this script again, started by the main thread at the render
 * worker's word, since a worker can't start one in every browser (the Claude app's browser pane
 * can't), and spoken to through a channel of its own. Its messages and failures come as a
 * `Worker`'s would. */
class Thread {
  static #next = 0;
  static readonly #all = new Map<number, Thread>();
  readonly #id = Thread.#next++;
  readonly #port: MessagePort;
  /** It failed, or couldn't start: why. */
  onerror: ((message: string) => void) | null = null;

  constructor(name: string) {
    const { port1, port2 } = new MessageChannel();
    this.#port = port1;
    Thread.#all.set(this.#id, this);
    self.postMessage({ type: "spawn", id: this.#id, name, port: port2 } satisfies FromWorker, [port2]);
  }

  set onmessage(f: ((event: MessageEvent) => void) | null) {
    this.#port.onmessage = f;
  }

  postMessage(message: ToWorker): void {
    this.#port.postMessage(message);
  }

  terminate(): void {
    if (!Thread.#all.delete(this.#id)) return;
    this.#port.close();
    self.postMessage({ type: "stop-thread", id: this.#id } satisfies FromWorker);
  }

  /** The main thread says thread `id` failed. */
  static failed(id: number, message: string): void {
    Thread.#all.get(id)?.onerror?.(message);
  }
}

/** The ticker's thread, once the program starts its ticker. A hot reload gives the new
 * build's ticker a new one. */
const ticker = {
  control: TickerControl.create(),
  memory: null as WebAssembly.Memory | null,
  /** Resolved with the ticks when the ticker's thread has stopped (test mode, a hot reload). */
  report: null as Promise<{ report: TickReport; log: Uint8Array<ArrayBuffer> | null; replay: Replay }> | null,
  thread: null as Thread | null,
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
function tickerStarter(
  wasm: Uint8Array,
  settings: TickSettings,
  replay: Replay | null = null,
): (options: TickerOptions, onPrint: PrintSink) => void {
  return (options, onPrint) => {
    const framesBeforeStart = ticker.control.frames;
    ticker.memory = options.memory;
    if (replay === null) ring?.attachTicker();
    const thread = new Thread("wrela sim");
    ticker.thread = thread;
    ticker.report = new Promise((resolve) => {
      thread.onmessage = (event: MessageEvent<FromTicker>) => {
        const msg = event.data;
        if (msg.type === "fatal") fatal(new Error(msg.message));
        else if (msg.type === "print") onPrint(msg.line);
        else resolve(msg);
      };
    });
    thread.onerror = (message) => fatal(new Error(`the ticker's thread failed: ${message}`));
    thread.postMessage({
      type: "ticker",
      ...options,
      ...settings,
      control: ticker.control.buffer,
      clock: clock.buffer,
      input: ring!.buffer,
      wasmHash: settings.hashes ? wasmHash(wasm) : 0n,
      replay,
      framesBeforeStart,
    } satisfies ToWorker);
  };
}

// ---- Hot reload ----

/** The running build's pipelines: the next build keeps those that are the same. */
const pipelineCache: PipelineCache = new Map();

/** The running build: what a hot reload replaced it with, or a hand-over gave its place to. */
let running: Build | null = null;

/** The helper threads the running program started: a hot reload stops them. */
const helpers: Thread[] = [];

/** What a replaced program hands on to its new build: what it kept, its input not yet read,
 * and its ticks to run again. */
interface Carried {
  kept: Uint8Array;
  inbox: Inbox;
  replay: Replay | null;
}

/** The options both ways of running give the program of build `b`: its requests, helpers,
 * voice and ticker (with `ticks`' settings), and what the build it replaces, if it does, hands
 * on (`carried`). */
function programOptions(b: Build, ticks: TickSettings, carried: Carried | null): ProgramOptions {
  return {
    io: browserIo(base, undefined, buildBase, requestHost, storageNamespace),
    builds: handOvers ?? undefined,
    spawnWorker,
    startVoice,
    startTicker: tickerStarter(b.wasm, ticks, carried?.replay ?? null),
    kept: carried?.kept,
    inbox: carried?.inbox,
    net,
  };
}

/** How a build is started, with what the program it replaces hands on, if it replaces one. */
type Start = (build: Build, carried: Carried | null) => Promise<Program>;

/** Stops `old`: its ticker stops (reporting its ticks, to run again, if `replaying`), its
 * helpers stop, and its buffers and textures go. What it hands on; the ticker's new words say
 * `frames` frames have started. */
async function stop(old: Program, frames: number, replaying: boolean): Promise<Carried> {
  let replay: Replay | null = null;
  if (ticker.report !== null) {
    const stopped = ticker.report;
    ticker.control.stop();
    if (replaying && old.ticker !== null) replay = (await stopped).replay;
    ticker.thread?.terminate();
  }
  old.shutdown();
  stopHelpers();
  (old.executor as GpuExecutor).destroyAll();
  newTicker(frames);
  return { kept: old.keeping, inbox: old.inbox, replay };
}

function stopHelpers(): void {
  for (const h of helpers.splice(0)) h.terminate();
}

/** A ticker's thread for the next program, none started yet: its words say `frames` frames have
 * started. */
function newTicker(frames: number): void {
  ticker.control = TickerControl.create();
  ticker.memory = null;
  ticker.report = null;
  ticker.thread = null;
  ticker.control.framesStarted(frames);
}

/** Stops `old` for the build at `base` (hot reload), which `start` starts with what the old one
 * hands on, its ticks to run again among it. */
async function swap(old: Program, base: string, frames: number, start: Start): Promise<Program> {
  const build = await loadBuild(base);
  buildBase = base;
  running = build;
  const program = await start(build, await stop(old, frames, true));
  // A ticker may disappear across a reload. Its reader must then stop holding the shared input
  // ring back; a replacement ticker attaches itself during its `init` instead.
  if (program.ticker === null) ring?.detachTicker();
  return program;
}

// ---- Hand-over ----

/** The builds the running program got ready, and the hand-over it asked for. */
let handOvers: HandOvers | null = null;

/** The pipelines of the build that handed over to the running one that the running one doesn't
 * use: going back to it builds none. */
let leftovers: PipelineCache = new Map();

/** Test mode keeps what the host says, as the program's printed lines (`results/log.txt`). */
let hostLine: ((line: string) => void) | null = null;

/** Hands `old`'s place to the build it asked for after its last frame (`std::reload::hand_over`),
 * once it's ready; `frames` frames have started. If it can't be got ready, the request fails
 * and `old` goes on. The new build starts with what `old` kept and its input not yet read (no
 * ticks: its sim is its own). If its `init` fails, `old`'s build starts again in its place with
 * what it kept: the screen never goes black. The program that runs after. */
async function handOver(old: Program, asked: Asked, frames: number, start: Start): Promise<Program> {
  let ready;
  try {
    ready = await handOvers!.ready(asked.base);
  } catch (e) {
    asked.fail(e);
    return old;
  }
  handOvers!.take(asked.base);
  const back = { build: running!, base: buildBase };
  const before = new Map(pipelineCache);
  if (old.hasVoice) self.postMessage({ type: "audio-stop" } satisfies FromWorker);
  const carried = await stop(old, frames, false);
  // The cache narrows to the new build's pipelines (`buildPipelines`): it offers them all, its
  // own got ready and those of the build before for the way back.
  for (const [key, p] of leftovers) if (!pipelineCache.has(key)) pipelineCache.set(key, p);
  for (const [key, p] of ready.pipelines) pipelineCache.set(key, p);
  buildBase = asked.base;
  try {
    const program = await start(ready, carried);
    if (program.ticker === null) ring?.detachTicker();
    running = ready;
    leftovers = new Map([...before].filter(([key]) => !pipelineCache.has(key)));
    return program;
  } catch (e) {
    // What its `init` started stops too.
    self.postMessage({ type: "audio-stop" } satisfies FromWorker);
    ticker.control.stop();
    ticker.thread?.terminate();
    stopHelpers();
    newTicker(frames);
    const line = `the hand-over to ${asked.base} failed: ${errorMessage(e)}; ${back.base} starts again`;
    console.warn(`wrela: ${line}`);
    hostLine?.(line);
    buildBase = back.base;
    pipelineCache.clear();
    for (const [key, p] of before) pipelineCache.set(key, p);
    const program = await start(back.build, carried);
    if (program.ticker === null) ring?.detachTicker();
    return program;
  }
}

// ---- Running normally ----

let size = { width: 1, height: 1 };

/** Hands the program's voice to the main thread, which plays it in an AudioWorklet: an
 * AudioContext exists only there. */
let voicePrint: PrintSink = () => {};
const startVoice = (voice: VoiceOptions, onPrint: PrintSink) => {
  voicePrint = onPrint;
  self.postMessage({ type: "audio", voice } satisfies FromWorker);
};

/** Helpers for parallel work and jobs: one per core beyond the main thread, the render worker
 * and the ticker's thread, at most MAX_WORKERS (#43 §2.1). The program's own thread counts too. */
const normalWorkers = () => Math.max(0, Math.min((navigator.hardwareConcurrency || 1) - 3, MAX_WORKERS)) + 1;

/** Runs the program, drawing the screen straight into the canvas in `format`. */
async function run(canvas: OffscreenCanvas, device: GPUDevice, format: GPUTextureFormat, build: Build, live: Live | null): Promise<void> {
  self.postMessage({ type: "screen-limit", max: device.limits.maxTextureDimension2D } satisfies FromWorker);
  const ctx = context(canvas, device, format, GPUTextureUsage.RENDER_ATTACHMENT);
  clock.startAt(epochNow());
  const screen = { format, texture: () => ctx.getCurrentTexture() };
  const start: Start = (b, carried) =>
    startProgram(
      device,
      b,
      screen,
      { ...programOptions(b, { ...NORMAL_TICKS, replayable: live !== null }, carried), workers: normalWorkers() },
      { cache: pipelineCache },
    );
  running = build;
  let program!: Program;
  // `p` runs from the next frame: its pipelines' failure is fatal, and its ticks go on the clock.
  const adopt = (p: Program) => {
    program = p;
    (p.executor as GpuExecutor).built.catch(fatal);
    ticker.control.go(TickerMode.CLOCK);
  };
  adopt(await start(build, null));
  const max = device.limits.maxTextureDimension2D;
  // A debug build's bounds checks: the flag is read after a frame, one read at a time.
  let checking = false;
  let firstPresented: Program | null = null;
  // Hot reload and hand-over: the frames drawn, and whether a new build is being swapped in (the
  // canvas keeps the last frame meanwhile).
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
          adopt(p);
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
        deliverInput(program, performance.timeOrigin + now);
        executor.presented = false;
        executor.frame = frames;
        program.frame(time, width, height);
        // A change is shown by the first frame that draws after it.
        if (executor.presented) {
          live?.drawn(frames);
          if (firstPresented !== program) { firstPresented = program; self.postMessage({ type: "presented" } satisfies FromWorker); }
        }
        frames++;
        if (!checking) {
          checking = true;
          executor.checkDebugFlag().then(() => (checking = false), fatal);
        }
        const asked = handOvers!.takeAsked();
        if (asked !== null) {
          swapping = true;
          handOver(program, asked, frames, start).then((p) => {
            if (p !== program) adopt(p);
            swapping = false;
          }, fatal);
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
  try {
    const encoder = device.createCommandEncoder();
    encoder.copyTextureToBuffer({ texture }, { buffer: staging, bytesPerRow: padded, rowsPerImage: height }, [width, height]);
    device.queue.submit([encoder.finish()]);
    await staging.mapAsync(GPUMapMode.READ);
    const mapped = new Uint8Array(staging.getMappedRange());
    const out = new Uint8Array(row * height);
    for (let y = 0; y < height; y++) out.set(mapped.subarray(y * padded, y * padded + row), y * row);
    staging.unmap();
    return out;
  } finally {
    staging.destroy();
  }
}

/** What a test records as it runs, for its results. */
interface TestRecord {
  /** What the program printed (`results/log.txt`), and the frame each line came in. */
  printed: string[];
  printed_in: number[];
  /** Each frame's CPU time (the program's `frame` and its batch's checks and encoding, up to
   * its submission), and when it began (ms since the epoch): `results/frames.json`. */
  cpu: number[];
  began_ms: number[];
  /** The latency test: when each frame started, and which events (by when they were sent) it got. */
  latency: { frames: number[]; delivered: { sent: number; frame: number }[] };
  /** How long the pipelines took to build. */
  pipelinesMs: number | null;
}

/** The program a test runs, and the frame its ticks count from: a build handed over to starts
 * its own, a hot reload's carries on. */
interface Running {
  program: Program;
  executor: GpuExecutor;
  hz: number;
  /** The first tick's frame, once a ticker has existed; null before then. */
  tickFrom: number | null;
}

/** `program`, running with its ticks counted from frame `tickFrom`. */
function runningFrom(program: Program, tickFrom: number | null): Running {
  const executor = program.executor as GpuExecutor;
  executor.built.catch(fatal);
  return { program, executor, hz: program.ticker?.hz ?? 0, tickFrom };
}

/** Publishes lockstep time before a frame can run, even when no tick is due yet and the
 * ticker's thread is still loading. Its thread later publishes the same origin. */
function scheduleTicker(mode: number, origin = 0): void {
  if (mode === TickerMode.LOCKSTEP && ticker.memory !== null) {
    new DataView(ticker.memory.buffer).setFloat64(TICK_ORIGIN, origin, true);
  }
  ticker.control.go(mode, origin);
}

/** `program` in the place of the one that ran, its ticker going on `schedule`. */
function runNext(program: Program, tickFrom: number | null, schedule: number, fps: number, frame: number): Running {
  const r = runningFrom(program, tickFrom ?? (program.ticker === null ? null : frame));
  if (r.hz > 0) scheduleTicker(schedule, r.tickFrom! / fps);
  return r;
}

async function runTest(canvas: OffscreenCanvas, device: GPUDevice, build: Build, params: TestParams, live: Live | null): Promise<void> {
  const { frames, width, height, fps } = params;
  const record: TestRecord = {
    printed: [],
    printed_in: [],
    cpu: [],
    began_ms: [],
    latency: { frames: [], delivered: [] },
    pipelinesMs: null,
  };
  let frameNow = 0;
  canvas.width = width;
  canvas.height = height;
  const ctx = context(canvas, device, SCREEN_FORMAT, GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_DST);
  // Screen passes render into a texture that can be read back; each is also copied to the
  // canvas so the page shows what ran.
  const screen = device.createTexture({
    label: "screen",
    size: [width, height],
    format: SCREEN_FORMAT,
    usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
  });
  const script: Scripted[] = params.input ? await loadScript(inputUrl(base, params.input)) : [];
  const frameEvents = arrivals(script).frames;
  const hashes = params.nohash === 0;
  // The tick log needs only the ticks' hashes (the sim's state, cheap), not the frames'.
  const tickHashes = hashes || params.ticklog > 0;
  const print = (line: string) => {
    record.printed.push(line);
    record.printed_in.push(frameNow);
  };
  hostLine = print;
  const salted = params.salt > 0 ? salt(build, params.salt) : build;
  const target: ScreenTarget = {
    format: SCREEN_FORMAT,
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
  const start: Start = (b, carried) =>
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
        onPrint: print,
      },
      {
        timing,
        onPipelines: (ms) => {
          record.pipelinesMs = ms;
        },
        cache: pipelineCache,
      },
    );
  running = salted;
  const first = await start(salted, null);
  let r = runningFrom(first, first.ticker === null ? null : 0);
  if (timing !== "off" && !r.executor.timed) throw new GpuError("the test asks for timestamps, but this device has no timestamp queries");
  const schedule = params.paced > 0 ? TickerMode.CLOCK : TickerMode.LOCKSTEP;
  if (params.latency > 0 || params.keylatency > 0) {
    self.postMessage({ type: "latency-start", ms: ((frames - 2) * 1000) / fps } satisfies FromWorker);
  }
  const began0 = performance.now();
  clock.startAt(epochNow());
  if (r.hz > 0) {
    scheduleTicker(schedule);
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
    if (failed) return;
    // Hot reload: a new build replaces the program before this frame.
    const next = live?.take(r.program) ?? null;
    if (next !== null) {
      r = runNext(await swap(r.program, next, i, start), r.tickFrom, schedule, fps, i);
      live!.swapped();
    }
    const control = ticker.control;
    // A program may start its ticker from a frame after init. Its first ticks belong to
    // this frame, the first one that follows that start, rather than every prior frame.
    if (r.hz === 0 && r.program.ticker !== null) {
      r.hz = r.program.ticker.hz;
      r.tickFrom = i;
      scheduleTicker(schedule, i / fps);
    }
    r.executor.frame = i;
    control.framesStarted(i + 1);
    if (r.hz > 0 && params.paced === 0) {
      // Lockstep: the ticks before this frame run first, as on the native host.
      const ticks = lockstepTicks(i - r.tickFrom!, r.hz, fps);
      control.runTo(ticks);
      await control.ranTo(ticks);
      // The ticker's thread reports its trap (`tickerStarter`).
      if (control.failed) return;
    }
    if (failed) return;
    for (const e of frameEvents.get(i) ?? []) r.program.queueScripted(e);
    if (params.clip > 0 && i === params.clipfrom) self.postMessage({ type: "clip-start" } satisfies FromWorker);
    if (params.clip > 0 && i === params.clipfrom + params.clip) self.postMessage({ type: "clip-stop" } satisfies FromWorker);
    record.latency.frames.push(epochNow());
    for (const sent of deliverInput(r.program)) record.latency.delivered.push({ sent, frame: i });
    const began = performance.now();
    frameNow = i;
    record.began_ms.push(performance.timeOrigin + began);
    const { program, executor } = r;
    await scoped(device, () => {
      program.frame(frameTime(i, fps), width, height);
      if (params.framedelay > 0) holdFor(params.framedelay);
      record.cpu.push(performance.now() - began);
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
    // A hand-over the frame asked for: the build handed over to starts its ticks from the next.
    const asked = handOvers!.takeAsked();
    if (asked !== null) {
      const p = await handOver(program, asked, i + 1, start);
      if (p !== program) r = runNext(p, null, schedule, fps, i + 1);
    }
  }
  await draining;
  if (failed) return;
  await saveResults(device, build, params, r, screen, timing, record);
}

/** A test's results, once its frames have run (`results/`), then `DONE`. */
async function saveResults(
  device: GPUDevice,
  build: Build,
  params: TestParams,
  { program, executor, hz }: Running,
  screen: GPUTexture,
  timing: Timing,
  record: TestRecord,
): Promise<void> {
  const { frames, width, height } = params;
  // Where the pipelines' time went: each built again alone, from code no cache has seen.
  const again = params.salt > 0 ? salt(build, params.salt + 7919) : null;
  const each = again ? await timeEachPipeline(device, again.manifest, again.shaders, screen.format) : [];
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
  if (params.audio > 0 || params.live > 0) {
    if (!program.hasVoice) throw new Error("the test asks for audio, but the program started no voice");
    await audioRendered.promise;
  }
  if (params.latency > 0 || params.keylatency > 0) {
    await latencySent.promise;
    await putResult(base, "latency.json", JSON.stringify(record.latency));
  }
  if (params.clip > 0) {
    if (params.clipfrom + params.clip >= frames) self.postMessage({ type: "clip-stop" } satisfies FromWorker);
    await clipSaved.promise;
  }
  const { printed, cpu, began_ms, printed_in, pipelinesMs } = record;
  // The audio and ticker threads' print messages precede their completion messages, so their
  // lines are in the program's print sink before its log is serialized here.
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
  self.postMessage({ type: "load-query" } satisfies FromWorker);
  const page = await pageLoaded.promise;
  const resources = [...page.resources, ...loaded(["resource"])];
  const load = { opened_ms: page.opened_ms, first_frame_ms: began_ms[0] ?? null, resources };
  await putResult(base, "load.json", JSON.stringify(load));
  // An asynchronous device or voice failure may arrive while results are being saved. Its
  // DONE report is final; a later successful write must not replace it.
  if (failed) return;
  console.log(`wrela test: ${frames} frames at ${width}x${height}, state hash ${hash ? hash.hex() : "none"}`);
  await reportDone("ok");
}

/** Starts a worker thread for the program's parallel jobs: this script again, told to run them. */
const spawnWorker: SpawnWorker = (module, memory, index, onPrint) => {
  const thread = new Thread(`wrela thread ${index}`);
  helpers.push(thread);
  thread.onerror = (message) => fatal(new Error(`worker thread ${index} failed: ${message}`));
  thread.onmessage = (event: MessageEvent<FromHelper>) => onPrint(event.data.line);
  thread.postMessage({ type: "thread", module, memory, index } satisfies ToWorker);
};

/** Test mode: resolved when the main thread has rendered the voice and saved its samples. */
const audioRendered = Promise.withResolvers<void>();
// Its failure can arrive before saveResults waits for it. Keep it handled from creation while
// preserving the rejection for that later await.
void audioRendered.promise.catch(() => {});

/** Test mode, `latency`: resolved when the main thread has sent its events. */
const latencySent = Promise.withResolvers<void>();

/** Test mode, `clip`: resolved when the main thread has saved the clip. */
const clipSaved = Promise.withResolvers<void>();

/** Test mode: resolved with what the page loaded, when the main thread answers. */
const pageLoaded = Promise.withResolvers<{ opened_ms: number; resources: Loaded[] }>();

/** What a message to this script does: from the main thread, or, on a thread of the render
 * worker's, from the render worker over the channel `reply` answers on. */
function receive(msg: ToWorker, reply: (message: FromTicker | FromHelper) => void): void {
  switch (msg.type) {
    case "parent":
      // This is a thread the main thread started for the render worker: its work comes on this
      // channel.
      msg.port.onmessage = (e: MessageEvent<ToWorker>) => receive(e.data, (m) => msg.port.postMessage(m));
      return;
    case "thread-failed":
      Thread.failed(msg.id, msg.message);
      return;
    case "load":
      pageLoaded.resolve(msg);
      return;
    case "ticker": {
      // This is the ticker's thread: it runs until it's stopped, or the page goes away.
      const { type: _, ...start } = msg;
      void runTicker(start, reply);
      return;
    }
    case "audio-rendered":
      audioRendered.resolve();
      return;
    case "audio-failed":
      fatal(failAudioRender(audioRendered, msg.message));
      return;
    case "print":
      voicePrint(msg.line);
      return;
    case "latency-sent":
      latencySent.resolve();
      return;
    case "clip-saved":
      clipSaved.resolve();
      return;
    case "thread":
      // It runs until the program shuts it down, or the page goes away.
      void runWorker(msg.module, msg.memory, msg.index, (line) => reply({ type: "print", line }));
      return;
    case "start": {
      base = msg.base;
      storageNamespace = msg.storage;
      buildBase = msg.base;
      testing = msg.test !== null;
      ring = new InputRing(msg.input);
      size = { width: msg.width, height: msg.height };
      const test = msg.test;
      // The device and the build are fetched at once.
      const live = msg.live !== null ? new Live(base, msg.live) : null;
      live?.start();
      handed = new Handed(msg.given);
      Promise.all([openDevice(fatal, test?.features ?? null), loadBuild(base, handed.fetch((url, init) => fetch(url, init)))])
        .then(([device, build]) => {
          const format = screenFormat(test !== null);
          handOvers = new HandOvers(device, format, () => buildBase, () => [pipelineCache, leftovers]);
          return test ? runTest(msg.canvas, device, build, test, live) : run(msg.canvas, device, format, build, live);
        })
        .catch(fatal);
      return;
    }
    case "file":
      handed.give(msg.url, msg.bytes);
      return;
    case "host-reply": {
      const waiting = hostRequests.get(msg.id);
      if (waiting) { hostRequests.delete(msg.id); if (msg.error === null) waiting.resolve(msg.bytes); else waiting.reject(new Error(msg.error)); }
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
}

self.onmessage = (event: MessageEvent<ToWorker>) => receive(event.data, (m) => self.postMessage(m));
