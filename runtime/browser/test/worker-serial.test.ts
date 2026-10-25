// The render worker's serial timing mode in test mode, with CPU stand-ins for its GPU and program:
// each frame's counters are what that frame's commands did, once the serial drain has run them.
import { expect, test } from "bun:test";
import { InputRing } from "../src/input.ts";
import { TEST_DEFAULTS } from "../src/testmode.ts";
import type { ToWorker } from "../src/messages.ts";

test("serial timing counts each frame's commands once its drain has run them", async () => {
  const globals = globalThis as unknown as Record<string, unknown>;
  const saved = new Map(["self", "fetch", "__serialTest"].map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)]));
  const done = Promise.withResolvers<string>();
  let counters = "";
  // Frame n records n + 1 draws; the drain runs whatever has been recorded since the last one,
  // a while later, as the GPU would.
  const recorded: number[] = [];
  let frames = 0;
  let draws = 0;
  const executor = {
    built: Promise.resolve(), frame: 0, presented: false, timed: true, memory: { bytes: 0, peak: 0 },
    caughtUp: async () => {}, checkDebugFlag: async () => {}, destroyAll() {}, timings: async () => [],
    async drain() {
      const batch = recorded.splice(0);
      await Bun.sleep(5);
      for (const n of batch) draws += n;
    },
    takeCounters() {
      const out = { draws };
      draws = 0;
      return out;
    },
  };
  const device = {
    createTexture: () => ({ width: 1, height: 1 }), pushErrorScope() {}, popErrorScope: async () => null,
    createBuffer: () => ({ mapAsync: async () => {}, getMappedRange: () => new ArrayBuffer(256), unmap() {}, destroy() {} }),
    createCommandEncoder: () => ({ copyTextureToBuffer() {}, finish() {} }),
    queue: { onSubmittedWorkDone: async () => {}, submit() {} },
  };
  const self = {
    location: { href: "https://site.example/worker.js" },
    onmessage: null as ((e: MessageEvent<ToWorker>) => void) | null,
    postMessage(message: { type: string; message?: string }) {
      if (message.type === "fatal") done.resolve(message.message!);
      if (message.type === "load-query") self.onmessage!({ data: { type: "load", opened_ms: 0, resources: [] } } as unknown as MessageEvent<ToWorker>);
    },
  };
  try {
    globals.self = self;
    globals.fetch = async (url: URL, init?: RequestInit) => {
      if (url.pathname.endsWith("/results/counters.json")) counters = String(init?.body);
      if (url.pathname.endsWith("/results/DONE")) done.resolve(String(init?.body));
      return new Response("");
    };
    globals.__serialTest = {
      device,
      makeProgram: () => ({
        executor, ticker: null, keeping: new Uint8Array(), shutdown() {}, queueInput() {}, workerChunks: () => 0, takePhases: () => ({}),
        frame() {
          recorded.push(++frames);
        },
      }),
    };
    const bundle = await Bun.build({ entrypoints: [new URL("../src/worker.ts", import.meta.url).pathname], target: "browser", format: "esm", plugins: [{
      name: "worker serial dependencies", setup(build) {
        build.onResolve({ filter: /\/(loader|device|live)\.ts$|^\.\/(loader|device|live)\.ts$/ }, ({ path }) => ({ path, namespace: "serial-test" }));
        build.onLoad({ filter: /.*/, namespace: "serial-test" }, ({ path }) => ({ loader: "js", contents: path.endsWith("device.ts")
          ? "export class GpuError extends Error {} export async function openDevice() { return globalThis.__serialTest.device; } export function deviceFeatures() { return []; }"
          : path.endsWith("live.ts") ? "export class Live { start() {} take() { return null; } swapped() {} drawn() {} }"
          : "export async function loadBuild() { return { wasm: new Uint8Array(), manifest: { pipelines: [] } }; } export async function loadLines() { return null; } export async function loadScript() { return []; } export function browserIo() { return {}; } export class Handed { give() {} fetch(f) { return f; } } export function networkFetch() {} export async function startProgram() { return globalThis.__serialTest.makeProgram(); }" }));
      },
    }] });
    expect(bundle.success).toBe(true);
    await import(`data:text/javascript;base64,${Buffer.from(await bundle.outputs[0]!.text()).toString("base64")}`);
    self.onmessage!({ data: {
      type: "start", base: "https://site.example/", storage: null, live: null, given: [], canvas: { getContext: () => ({ configure() {} }) },
      input: InputRing.create().buffer, width: 1, height: 1, test: { ...TEST_DEFAULTS, frames: 4, width: 1, height: 1, fps: 60, saturate: 1, timestamps: 2 },
    } } as unknown as MessageEvent<ToWorker>);
    expect(await Promise.race([done.promise, Bun.sleep(2000).then(() => "timed out")])).toBe("ok");
    expect(JSON.parse(counters)).toEqual([1, 2, 3, 4].map((draws) => ({ draws })));
  } finally {
    for (const [name, descriptor] of saved) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else Reflect.deleteProperty(globalThis, name);
    }
  }
});
