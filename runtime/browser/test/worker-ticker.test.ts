// The render worker's ticker scheduling and failures, with CPU stand-ins for its GPU and program.
import { expect, test } from "bun:test";
import { InputRing } from "../src/input.ts";
import { SCREEN_FORMAT, TICK_ORIGIN } from "../src/abi.gen.ts";
import { Inbox } from "../src/net.ts";
import { TickerControl } from "../src/ticker.ts";
import { TEST_DEFAULTS } from "../src/testmode.ts";
import type { TickerStart, ToWorker } from "../src/messages.ts";

let scenarios = 0;

async function tickerScenario(reloading: boolean, history = false, hz = 60, asynchronousFailure: "frame" | "results" | "report" | "readback" | null = null): Promise<void> {
  const globals = globalThis as unknown as Record<string, unknown>;
  const saved = new Map(["self", "fetch", "__tickerTest"].map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)]));
  const timers: ReturnType<typeof setInterval>[] = [];
  const targets: number[] = [];
  const origins: number[] = [];
  const startedAfter: number[] = [];
  const frameOrigins: number[] = [];
  let frames = 0;
  let builds = 0;
  let buffersDestroyed = 0;
  let reloaded = false;
  const stopped = Promise.withResolvers<string>();
  const reported = Promise.withResolvers<void>();
  const done: string[] = [];
  const configured: GPUTextureFormat[] = [];
  const executor = { built: Promise.resolve(), frame: 0, presented: false, caughtUp: async () => {}, checkDebugFlag: async () => {}, destroyAll() {}, takeCounters: () => ({}) };
  const device = {
    createTexture: () => ({ width: 1, height: 1 }), pushErrorScope() {}, popErrorScope: async () => null,
    createBuffer: () => ({
      mapAsync: async () => { if (asynchronousFailure === "readback") throw new Error("readback mapping failed"); },
      getMappedRange: () => new ArrayBuffer(256), unmap() {}, destroy() { buffersDestroyed++; },
    }),
    createCommandEncoder: () => ({ copyTextureToBuffer() {}, finish() {} }),
    queue: { onSubmittedWorkDone: async () => {}, submit() {} },
  };
  /** The ticker's thread, on its channel to the render worker: it runs whatever ticks the
   * control asks for, and reports when it's stopped. */
  const fakeTicker = (port: MessagePort) => {
    port.onmessage = (e: MessageEvent<TickerStart>) => {
      const start = e.data;
      const control = new TickerControl(start.control);
      startedAfter.push(start.framesBeforeStart);
      let reported = false;
      timers.push(setInterval(() => {
        if (control.stopped) {
          if (!reported) port.postMessage({
            type: "ticks", report: { cpu_ms: [] }, log: null, replay: { upto: control.target, ticks: [], origin: control.origin },
          });
          reported = true;
          return;
        }
        if (control.target > 0) { targets.push(control.target); origins.push(control.origin); control.ran(control.target); }
      }, 1));
    };
  };
  const self = {
    location: { href: "https://site.example/worker.js" },
    onmessage: null as ((e: MessageEvent<ToWorker>) => void) | null,
    postMessage(message: { type: string; message?: string; port?: MessagePort }) {
      if (message.type === "fatal") stopped.resolve(message.message!);
      // The main thread starts the ticker's thread and hands it its channel (`spawn`).
      if (message.type === "spawn") fakeTicker(message.port!);
      if (message.type === "load-query" && (asynchronousFailure === "results" || asynchronousFailure === "report")) {
        if (asynchronousFailure === "results") self.onmessage!({ data: { type: "audio-failed", message: "audio stopped during results" } } as MessageEvent<ToWorker>);
        self.onmessage!({ data: { type: "load", opened_ms: 0, resources: [] } } as unknown as MessageEvent<ToWorker>);
      }
    },
  };
  try {
    globals.self = self;
    globals.fetch = async (url: URL, init?: RequestInit) => {
      if (url.pathname.endsWith("/results/DONE")) {
        const body = String(init?.body);
        if (body === "ok" && asynchronousFailure === "report") {
          self.onmessage!({ data: { type: "audio-failed", message: "audio stopped during results" } } as MessageEvent<ToWorker>);
          // The successful PUT is already in flight when the error is reported. Its server
          // write finishes later than the error's unless the worker serializes both writes.
          await Bun.sleep(20);
        }
        done.push(body);
        if (body !== "ok") reported.resolve();
      }
      return new Response("");
    };
    globals.__tickerTest = { device, takeReload() {
      if (!reloading || reloaded || frames < 2) return null;
      reloaded = true;
      return "https://site.example/next/";
    }, makeProgram(options: { startTicker: (options: object, print: () => void) => void }) {
      const replacement = builds++ > 0;
      const memory = new WebAssembly.Memory({ initial: 20, maximum: 16384, shared: true });
      const program = {
        executor, ticker: null as { hz: number } | null, inbox: new Inbox(), queueInput() {},
        keeping: new Uint8Array(), shutdown() {}, workerChunks: () => 0, takePhases: () => ({}),
        frame() {
          if (asynchronousFailure === "frame" && frames === 0) queueMicrotask(() => self.onmessage!({ data: {
            type: "audio-failed", message: "audio stopped during the frame",
          } } as MessageEvent<ToWorker>));
          if (!reloading && asynchronousFailure === null && frames === 1) {
            program.ticker = { hz };
            options.startTicker({ hz, memory }, () => {});
          }
          if (frames === 2 && program.ticker !== null) frameOrigins.push(new DataView(memory.buffer).getFloat64(TICK_ORIGIN, true));
          if (frames++ === 2 && asynchronousFailure === null) throw new Error("stopped after three frames");
        },
      };
      if (replacement || history) {
        program.ticker = { hz };
        options.startTicker({ hz, memory }, () => {});
      }
      return program;
    } };
    const bundle = await Bun.build({ entrypoints: [new URL("../src/worker.ts", import.meta.url).pathname], target: "browser", format: "esm", plugins: [{
      name: "worker ticker dependencies", setup(build) {
        build.onResolve({ filter: /\/(loader|device|live)\.ts$|^\.\/(loader|device|live)\.ts$/ }, ({ path }) => ({ path, namespace: "ticker-test" }));
        build.onLoad({ filter: /.*/, namespace: "ticker-test" }, ({ path }) => ({ loader: "js", contents: path.endsWith("device.ts")
          ? "export class GpuError extends Error {} export async function openDevice() { return globalThis.__tickerTest.device; } export function deviceLimits() { return []; }"
          : path.endsWith("live.ts") ? "export class Live { start() {} take() { return globalThis.__tickerTest.takeReload(); } swapped() {} drawn() {} }"
          : "export async function loadBuild() { return { wasm: new Uint8Array(), manifest: { pipelines: [] } }; } export async function loadLines() { return null; } export async function loadScript() { return []; } export function browserIo() { return {}; } export class Handed { give() {} fetch(f) { return f; } } export async function startProgram(_d, _b, _s, options) { return globalThis.__tickerTest.makeProgram(options); }" }));
      },
    }] });
    expect(bundle.success).toBe(true);
    const source = `${await bundle.outputs[0]!.text()}\n// ticker scenario ${scenarios++}\n`;
    await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
    self.onmessage!({ data: {
      type: "start", base: "https://site.example/", storage: null, live: reloading ? 0 : null, given: [], canvas: { getContext: () => ({ configure: (c: GPUCanvasConfiguration) => configured.push(c.format) }) },
      input: InputRing.create().buffer, width: 1, height: 1, test: { ...TEST_DEFAULTS, frames: 4, width: 1, height: 1, fps: 60, saturate: 1 },
    } } as unknown as MessageEvent<ToWorker>);
    const failure = await Promise.race([stopped.promise, Bun.sleep(2000).then(() => "timed out")]);
    // Test mode reads the screen back in SCREEN_FORMAT and copies it to the canvas, whatever
    // format the device prefers.
    expect(configured).toEqual([SCREEN_FORMAT]);
    expect(await Promise.race([reported.promise.then(() => true), Bun.sleep(2000).then(() => false)])).toBe(true);
    if (asynchronousFailure !== null) {
      expect(failure).toContain(asynchronousFailure === "frame" ? "audio stopped during the frame" : asynchronousFailure === "readback" ? "readback mapping failed" : "audio stopped during results");
      // Let the current asynchronous work finish. No later frame or successful DONE may follow.
      await Bun.sleep(asynchronousFailure === "report" ? 50 : 20);
      expect(frames).toBe(asynchronousFailure === "frame" ? 1 : 4);
      expect(done).toEqual(asynchronousFailure === "report" ? ["ok", failure] : [failure]);
      if (asynchronousFailure === "readback") expect(buffersDestroyed).toBe(1);
      return;
    }
    expect(failure).toContain("stopped after three frames");
    // The ticker's thread hears the render worker on a channel: let what's on it arrive.
    await Bun.sleep(10);
    expect(frames).toBe(3);
    expect(targets).toEqual(history ? [1, 2, 3] : hz === 60 ? [1] : []);
    expect(origins).toEqual(history ? [0, 0, 0] : hz === 60 ? [2 / 60] : []);
    expect(startedAfter).toEqual(history ? [0, 2] : [2]);
    // The stand-in ticker never writes the program's memory: it may still be loading when
    // a zero-target frame returns from ranTo(0), so the render worker must publish this.
    if (hz < 60) expect(frameOrigins).toEqual([2 / 60]);
  } finally {
    for (const timer of timers) clearInterval(timer);
    for (const [name, descriptor] of saved) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else Reflect.deleteProperty(globalThis, name);
    }
  }
}

test("a ticker first started during a frame gets ticks from the next frame", () => tickerScenario(false));

test("a reload starting the first ticker counts from the reload frame", () => tickerScenario(true));

test("a reload with ticker history keeps its original schedule", () => tickerScenario(true, true));

test("a zero-tick frame sees its origin before the ticker thread starts", () => tickerScenario(false, false, 30));

test("an asynchronous fatal error stops test frames", () => tickerScenario(false, false, 60, "frame"));

test("a fatal error during result saving cannot be replaced by a successful DONE", () => tickerScenario(false, false, 60, "results"));

test("a fatal error during a successful DONE upload stays the final result", () => tickerScenario(false, false, 60, "report"));

test("a failed result readback destroys its staging buffer", () => tickerScenario(false, false, 60, "readback"));
